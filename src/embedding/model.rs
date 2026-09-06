// Re-export from domain (canonical source)
pub use crate::domain::EMBEDDING_DIM;

/// Marker file recording which model content a model dir holds. Written by
/// `verify_model_dir` only after the weights hash AND the companion files check
/// pass, so its presence means "this exact build blessed this directory".
pub const MODEL_ID_MARKER: &str = ".model-id";

/// Pure core of `EmbeddingModel::model_files_state` — `"absent"`, `"unverified"`
/// or `"ready"` for one candidate directory.
///
/// Lives outside the `embed-model` gate deliberately: the decision is path logic
/// with no tensor in it, and gating it would leave the branch that produces
/// user-facing advice testable only in the heavyweight feature build — which is
/// how the advice drifted from what the downloader actually does in the first
/// place.
///
/// `is_platform_cache` is the whole subtlety. Weights in the platform cache dir
/// are subject to the downloader's currency check, so ones that do not match this
/// build get replaced on the next server start rather than used. Weights anywhere
/// else — `CODE_GRAPH_MODEL_DIR`, `cwd/models`, `exe/models`, the documented
/// offline routes — are loaded as-is and are therefore `ready` whatever marker
/// they carry.
pub fn model_files_state_for(
    dir: &std::path::Path,
    is_platform_cache: bool,
    expected_model_id: &str,
    companion_files: &[&str],
) -> &'static str {
    if !dir.join("model.safetensors").exists() {
        return "absent";
    }
    if !is_platform_cache {
        return "ready";
    }
    let companions_present = companion_files.iter().all(|f| dir.join(f).exists());
    let marker_current = std::fs::read_to_string(dir.join(MODEL_ID_MARKER))
        .map(|id| id.trim() == expected_model_id)
        .unwrap_or(false);
    if companions_present && marker_current {
        "ready"
    } else {
        "unverified"
    }
}

#[cfg(feature = "embed-model")]
mod inner {
    use anyhow::Result;
    use candle_core::{DType, Device, Tensor};
    use candle_nn::VarBuilder;
    use candle_transformers::models::bert::{BertModel, Config};
    use tokenizers::Tokenizer;

    pub struct EmbeddingModel {
        model: BertModel,
        tokenizer: Tokenizer,
        device: Device,
    }

    impl EmbeddingModel {
        /// Load the embedding model. Returns Ok(None) if model files are not available
        /// (graceful degradation to FTS5-only search).
        pub fn load() -> Result<Option<Self>> {
            match Self::try_load() {
                Ok(m) => {
                    tracing::info!("Embedding model loaded successfully");
                    Ok(Some(m))
                }
                Err(e) => {
                    tracing::warn!(
                        "Embedding model not available, falling back to FTS5-only: {}",
                        e
                    );
                    Ok(None)
                }
            }
        }

        fn try_load() -> Result<Self> {
            let device = Device::Cpu;

            let (model_data, tokenizer_data, config_data) = Self::load_model_data()?;

            let config: Config = serde_json::from_slice(&config_data)?;
            let vb = VarBuilder::from_buffered_safetensors(model_data, DType::F32, &device)?;
            let model = BertModel::load(vb, &config)?;

            let mut tokenizer = Tokenizer::from_bytes(&tokenizer_data)
                .map_err(|e| anyhow::anyhow!("tokenizer load error: {}", e))?;

            // Truncate to 512 tokens — context strings include code_content, calls, callers, imports, etc.
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: 512,
                    ..Default::default()
                }))
                .map_err(|e| anyhow::anyhow!("truncation config error: {}", e))?;

            Ok(Self {
                model,
                tokenizer,
                device,
            })
        }

        fn load_model_data() -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
            let models_dir = Self::find_models_dir()?;
            Ok((
                std::fs::read(models_dir.join("model.safetensors"))?,
                std::fs::read(models_dir.join("tokenizer.json"))?,
                std::fs::read(models_dir.join("config.json"))?,
            ))
        }

        /// Platform-specific cache directory for model files.
        pub fn cache_models_dir() -> Result<std::path::PathBuf> {
            let cache = dirs::cache_dir()
                .ok_or_else(|| anyhow::anyhow!("Cannot determine cache directory"))?;
            Ok(cache.join("code-graph").join("models"))
        }

        /// Where the last download outcome is recorded. Sits BESIDE the models
        /// dir, not inside it, so `extract_and_promote`'s rename dance can't
        /// take the diagnostics with it.
        pub fn download_state_file() -> Result<std::path::PathBuf> {
            Ok(Self::cache_models_dir()?
                .parent()
                .ok_or_else(|| anyhow::anyhow!("model cache dir has no parent"))?
                .join("model-download.json"))
        }

        /// Persist the outcome of a download attempt.
        ///
        /// The background download previously logged to `tracing` only. A user
        /// whose download never succeeded saw one `WARN … will be downloaded on
        /// first use` and nothing else — not whether an attempt was made, what
        /// it returned, or why it stopped (issue #35) — while `doctor` kept
        /// printing "retry shortly" forever, which reads as *be patient* rather
        /// than *this is broken*. Writing the outcome where `doctor` and the
        /// search tools can read it is the fix that stands regardless of what
        /// makes a given machine's download fail.
        ///
        /// Best-effort: a diagnostics write must never break the download path,
        /// so every failure here is swallowed.
        pub fn record_download_state(status: &str, attempts: u32, error: Option<&str>) {
            Self::record_download_state_trusted(status, attempts, error, None);
        }

        /// [`record_download_state`] plus the TLS trust path that produced the
        /// bytes (`"bundled"` / `"platform"`).
        ///
        /// Recorded because the OS-trust-store fallback is otherwise invisible
        /// after the fact: nothing on disk distinguished a model fetched through
        /// ureq's pinned webpki roots from one fetched through whatever the
        /// machine's certificate store happened to contain (a corporate MITM
        /// root, an enterprise MDM profile), so `doctor` could not answer the
        /// question and neither could the user. Content integrity does not
        /// depend on this — the blake3 pin in `extract_and_promote` is checked
        /// either way — but provenance is worth being able to state.
        pub fn record_download_state_trusted(
            status: &str,
            attempts: u32,
            error: Option<&str>,
            trust_path: Option<&str>,
        ) {
            let Ok(path) = Self::download_state_file() else {
                return;
            };
            Self::record_download_state_at_trusted(&path, status, attempts, error, trust_path);
        }

        /// Testable core of [`record_download_state`], parameterized on the state
        /// file path.
        ///
        /// The path is injected rather than redirected through the environment
        /// because `dirs::cache_dir()` only honors `XDG_CACHE_HOME` on Linux/BSD:
        /// on macOS it is unconditionally `$HOME/Library/Caches` and on Windows it
        /// comes from `SHGetKnownFolderPath`, not the `LOCALAPPDATA` variable. An
        /// env-redirecting test helper is therefore a silent no-op on two of the
        /// three shipped platforms — it writes the developer's REAL
        /// `model-download.json`, which both corrupts their diagnostics and makes
        /// the test self-poisoning (the second run sees the first run's status
        /// where it asserts "never attempted"). Injection also removes the
        /// `env::set_var` that raced sibling tests on the same harness thread pool.
        pub fn record_download_state_at(
            path: &std::path::Path,
            status: &str,
            attempts: u32,
            error: Option<&str>,
        ) {
            Self::record_download_state_at_trusted(path, status, attempts, error, None);
        }

        /// [`record_download_state_at`] with the TLS trust path — see
        /// [`record_download_state_trusted`].
        pub fn record_download_state_at_trusted(
            path: &std::path::Path,
            status: &str,
            attempts: u32,
            error: Option<&str>,
            trust_path: Option<&str>,
        ) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let state = serde_json::json!({
                "status": status,
                "attempts": attempts,
                "error": error,
                "updated_epoch": epoch,
                "url": Self::model_download_url(),
                "binary_version": env!("CARGO_PKG_VERSION"),
                "trust_path": trust_path,
            });
            let _ = std::fs::write(path, serde_json::to_vec_pretty(&state).unwrap_or_default());
        }

        /// Human-readable summary of the last download attempt for `doctor` and
        /// the FTS5-only search note. `None` when no attempt was ever recorded —
        /// itself a diagnosis ("never attempted"), distinct from "failed".
        pub fn download_state_summary() -> Option<String> {
            Self::download_state_summary_at(&Self::download_state_file().ok()?)
        }

        /// Testable core of [`download_state_summary`]. See
        /// [`record_download_state_at`] for why the path is injected.
        pub fn download_state_summary_at(path: &std::path::Path) -> Option<String> {
            let raw = std::fs::read_to_string(path).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let status = v["status"].as_str().unwrap_or("unknown");
            let attempts = v["attempts"].as_u64().unwrap_or(0);
            Some(match status {
                "ok" => "model downloaded successfully".to_string(),
                "in_flight" => format!("download in flight (attempt {})", attempts.max(1)),
                "disabled" => {
                    "download disabled by CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1".to_string()
                }
                "failed" => format!(
                    "download FAILED after {} attempt(s): {}",
                    attempts,
                    v["error"].as_str().unwrap_or("unknown error")
                ),
                other => other.to_string(),
            })
        }

        /// blake3 of the pinned model.safetensors content
        /// (sentence-transformers/all-MiniLM-L6-v2 @ HF revision c9745ed1d9f2,
        /// sha256 53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db).
        /// MUST be bumped in lockstep with HF_REVISION in
        /// .github/workflows/release.yml ("Package model files" step) — a
        /// mismatched constant makes every client reject the released tarball.
        pub const MODEL_CONTENT_BLAKE3: &'static str =
            "8087e9bf97c265f8435ed268733ecf3791825ad24850fd5d84d89e32ee3a589a";

        /// Marker file recording which model content the cache dir holds.
        /// Defined outside the feature gate so the ungated state probe
        /// (`super::model_files_state_for`) reads the same filename.
        const MODEL_ID_MARKER: &'static str = super::MODEL_ID_MARKER;

        /// Files the loader (`load_model_data`) reads BEYOND the hash-pinned weights.
        /// A cache dir with correct `model.safetensors` but missing either of these is
        /// unusable — `try_load` fails and `load()` degrades to FTS5-only. They are
        /// checked before the `.model-id` marker is written so a partial extraction
        /// (killed after the weights but before these) can never be blessed as current.
        const REQUIRED_COMPANION_FILES: [&'static str; 2] = ["tokenizer.json", "config.json"];

        fn hash_file_blake3(path: &std::path::Path) -> Result<String> {
            use std::io::Read as IoRead;
            let mut hasher = blake3::Hasher::new();
            let mut f = std::fs::File::open(path)?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(hasher.finalize().to_hex().to_string())
        }

        /// Verify that `dir/model.safetensors` matches `expected` (blake3 hex).
        /// On success writes the `.model-id` marker so later checks are O(1);
        /// on mismatch removes the bad weights so a half-poisoned cache can't
        /// be loaded later, and returns an error.
        pub fn verify_model_dir(dir: &std::path::Path, expected: &str) -> Result<()> {
            let weights = dir.join("model.safetensors");
            let actual = Self::hash_file_blake3(&weights)?;
            if actual != expected {
                let _ = std::fs::remove_file(&weights);
                let _ = std::fs::remove_file(dir.join(Self::MODEL_ID_MARKER));
                anyhow::bail!(
                    "model.safetensors content mismatch (blake3 {} != expected {}) — rejected",
                    actual,
                    expected
                );
            }
            // Correct weights are necessary but NOT sufficient — the loader also needs the
            // companion files. Refuse to write the marker (which blesses the dir as current
            // and short-circuits re-download) until the dir is complete, so a partial
            // extraction can't pin a half-populated, unloadable cache.
            for f in Self::REQUIRED_COMPANION_FILES {
                if !dir.join(f).exists() {
                    let _ = std::fs::remove_file(dir.join(Self::MODEL_ID_MARKER));
                    anyhow::bail!(
                        "model dir incomplete: missing {} (partial download?) — rejected",
                        f
                    );
                }
            }
            std::fs::write(dir.join(Self::MODEL_ID_MARKER), expected)?;
            Ok(())
        }

        /// Version/identity-aware cache check (testable core; `expected` is the
        /// blake3 the running binary requires). True iff the cached weights are
        /// present, match, AND the loader's companion files are present. A marker hit
        /// short-circuits the hash (but still re-checks companions, so a dir that LOSES
        /// tokenizer.json/config.json after blessing — external mutation / AV quarantine —
        /// is reported not-current and re-downloaded instead of stranding FTS5-only). A
        /// missing marker (pre-v0.50 install) triggers a one-time hash, writing the marker
        /// on match. Unreadable weights → treat as current (never churn-redownload on a
        /// flaky FS; load() surfaces the real error).
        pub fn cached_model_matches(dir: &std::path::Path, expected: &str) -> bool {
            let weights = dir.join("model.safetensors");
            if !weights.exists() {
                return false;
            }
            // "Current" must imply "loadable": a complete dir needs the companions too.
            if !Self::REQUIRED_COMPANION_FILES
                .iter()
                .all(|f| dir.join(f).exists())
            {
                return false;
            }
            let marker = dir.join(Self::MODEL_ID_MARKER);
            if let Ok(id) = std::fs::read_to_string(&marker) {
                return id.trim() == expected;
            }
            match Self::hash_file_blake3(&weights) {
                Ok(h) if h == expected => {
                    // Companions already confirmed present above — safe to bless this
                    // pre-marker (pre-v0.50) cache by writing the one-time marker.
                    let _ = std::fs::write(&marker, expected);
                    true
                }
                Ok(_) => false,
                Err(_) => true,
            }
        }

        /// Is the cached model the one THIS binary expects? Existence alone is
        /// not enough: the existence-only check left the model permanently
        /// pinned to whatever was downloaded first (same fault class as the
        /// native-binary pin fixed in v0.45.x) and accepted any content that
        /// happened to sit in the cache dir.
        pub fn cached_model_is_current(dir: &std::path::Path) -> bool {
            Self::cached_model_matches(dir, Self::MODEL_CONTENT_BLAKE3)
        }

        /// URL for model download, based on current binary version.
        pub fn model_download_url() -> String {
            let version = env!("CARGO_PKG_VERSION");
            format!(
                "https://github.com/sdsrss/code-graph-mcp/releases/download/v{}/models.tar.gz",
                version
            )
        }

        /// Unpack an in-memory model tar.gz into `dest_dir` ATOMICALLY: extract to a
        /// per-process staging dir, verify the weights hash (`expected`) AND companion-file
        /// completeness, then rename into place (rolling the previous cache back on a failed
        /// promote). Returns Err WITHOUT mutating `dest_dir` on any failure — bad hash,
        /// incomplete archive, or a path-traversal entry — so a partial/poisoned download can
        /// never replace a good cache, and a kill mid-extraction leaves only the staging dir.
        ///
        /// A testable seam: production passes `MODEL_CONTENT_BLAKE3`; tests pass the blake3 of
        /// their fixture weights so the success-promote path can be exercised without the real
        /// (un-forgeable-hash) model. `pub` only so the sibling test module can drive it.
        pub(crate) fn extract_and_promote(
            body: &[u8],
            dest_dir: &std::path::Path,
            expected: &str,
        ) -> Result<()> {
            // The published bundle unpacks to ~90 MB; 400 MiB leaves a 4x margin
            // for the model growing while still bounding a decompression bomb.
            Self::extract_and_promote_capped(body, dest_dir, expected, 400 * 1024 * 1024)
        }

        /// Testable core of [`extract_and_promote`] — `max_unpacked` is a
        /// parameter so the refusal can be exercised on a kilobyte fixture
        /// instead of a gigabyte one.
        pub(crate) fn extract_and_promote_capped(
            body: &[u8],
            dest_dir: &std::path::Path,
            expected: &str,
            max_unpacked: u64,
        ) -> Result<()> {
            let parent = dest_dir
                .parent()
                .ok_or_else(|| anyhow::anyhow!("model cache dir has no parent: {:?}", dest_dir))?;
            std::fs::create_dir_all(parent)?;

            // GC orphaned staging/old dirs from prior runs killed abnormally. StagingGuard
            // (below) only fires on a graceful unwind — a SIGKILL/OOM-kill/power-loss, or a
            // release-build panic (`panic = "abort"` skips Drop), leaves a model-sized dir
            // with no other reclaimer, accumulating across version bumps. PID-liveness isn't
            // portable here, so use age: a staging/old dir older than this threshold cannot be
            // an in-flight concurrent download (those complete in seconds), so removing it is
            // safe even while another instance is mid-download.
            const ORPHAN_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);
            if let Ok(rd) = std::fs::read_dir(parent) {
                let now = std::time::SystemTime::now();
                for ent in rd.flatten() {
                    let name = ent.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with(".models-staging.") || name.starts_with(".models-old.") {
                        let orphaned = ent
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| now.duration_since(t).ok())
                            .is_some_and(|age| age > ORPHAN_MAX_AGE);
                        if orphaned {
                            let _ = std::fs::remove_dir_all(ent.path());
                        }
                    }
                }
            }

            let staging = parent.join(format!(".models-staging.{}", std::process::id()));

            // RAII: always remove the staging dir on any exit (error OR after a successful
            // rename, where it no longer exists and the removal harmlessly no-ops). The
            // PID-scoped name also isolates concurrent multi-instance downloads so they can't
            // interleave writes into one shared dir and tear the weights.
            struct StagingGuard<'a>(&'a std::path::Path);
            impl Drop for StagingGuard<'_> {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(self.0);
                }
            }
            let _ = std::fs::remove_dir_all(&staging); // clear any aborted prior staging
            std::fs::create_dir_all(&staging)?;
            let _staging_guard = StagingGuard(&staging);

            // Extract tar.gz with path traversal protection
            let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(body));
            let mut archive = tar::Archive::new(gz);
            archive.set_overwrite(true);
            // Extraction happens BEFORE the blake3 pin can be checked (the pin is
            // over the unpacked weights), so the bytes written here are not yet
            // trusted. The 200 MB cap in `download_model_to` bounds the
            // COMPRESSED body; gzip's ratio means that still permits a
            // multi-gigabyte unpack, and the disk it fills is the user's. Bound
            // the decompressed side too.
            let mut unpacked: u64 = 0;
            for entry in archive.entries()? {
                let mut entry = entry?;
                let path = entry.path()?;
                // Reject entries with path traversal components
                if path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    anyhow::bail!("Tar entry contains path traversal: {:?}", path);
                }
                // Checked against the header BEFORE unpacking, so an oversized
                // member is refused rather than written and then noticed.
                unpacked = unpacked.saturating_add(entry.header().size()?);
                if unpacked > max_unpacked {
                    anyhow::bail!(
                        "Model archive unpacks to more than {} bytes — refusing to fill the disk \
                         with unverified data",
                        max_unpacked
                    );
                }
                entry.unpack_in(&staging)?;
            }

            // Content verification against the pin. The release also publishes
            // models.tar.gz.sha256, but that asset only self-validates the bundle — a swapped
            // GitHub release asset would ship a matching .sha256. The binary pinning the
            // expected weights hash is the only check the attacker can't ship alongside.
            // verify_model_dir ALSO enforces companion-file completeness in staging.
            Self::verify_model_dir(&staging, expected)?;

            // Write version marker (blake3 of tarball for cache invalidation)
            let hash = blake3::hash(body);
            std::fs::write(staging.join(".version"), hash.to_hex().as_str())?;

            // Atomically promote staging → dest_dir. rename onto a non-empty dir fails
            // (ENOTEMPTY), so move any existing cache aside first. The brief window where
            // dest_dir is absent is safe: a concurrent reader just sees no model and runs
            // FTS5-only until the next call. On promote failure, roll the old dir back so we
            // never leave the cache with NO model.
            if dest_dir.exists() {
                let old = parent.join(format!(".models-old.{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&old);
                std::fs::rename(dest_dir, &old)?;
                match std::fs::rename(&staging, dest_dir) {
                    Ok(()) => {
                        let _ = std::fs::remove_dir_all(&old);
                    }
                    Err(e) => {
                        let _ = std::fs::rename(&old, dest_dir); // restore previous cache
                        return Err(
                            anyhow::Error::from(e).context("promoting staged model into cache")
                        );
                    }
                }
            } else {
                std::fs::rename(&staging, dest_dir)?;
            }
            Ok(())
        }

        /// Download model tarball from URL with timeout, extract to dest_dir.
        /// Integrity: HTTPS transport + valid tar.gz extraction + blake3 version marker.
        /// Overall budget for the whole request. The released `models.tar.gz` is
        /// ~83 MB (not the "~30 MB compressed" an earlier comment claimed), so
        /// the previous 120s ceiling silently demanded ~700 KB/s sustained and
        /// turned any slower link into a hard failure (issue #35).
        const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

        fn agent_with_roots(platform_verifier: bool) -> ureq::Agent {
            let builder =
                ureq::config::Config::builder().timeout_global(Some(Self::DOWNLOAD_TIMEOUT));
            let config = if platform_verifier {
                builder
                    .tls_config(
                        ureq::tls::TlsConfig::builder()
                            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                            .build(),
                    )
                    .build()
            } else {
                builder.build()
            };
            ureq::Agent::new_with_config(config)
        }

        /// Can a DIFFERENT root store plausibly turn this failure into a success?
        ///
        /// The OS-trust-store retry exists for one shape: a TLS-inspecting proxy
        /// whose private root is not in ureq's bundled webpki set. Retrying
        /// anything else buys nothing and costs a second full
        /// [`DOWNLOAD_TIMEOUT`] — with the caller's 3 attempts that turned a dead
        /// mirror into 3 × 2 × 600s ≈ one hour of `model-download.json` reading
        /// "in flight", which `doctor` reports verbatim to a user who could
        /// otherwise have been told the download failed 55 minutes earlier.
        pub(crate) fn os_trust_store_might_help(e: &ureq::Error) -> bool {
            // Nothing about the root store changes these.
            if matches!(
                e,
                ureq::Error::Timeout(_)
                    | ureq::Error::StatusCode(_)
                    | ureq::Error::HostNotFound
                    | ureq::Error::BadUri(_)
                    | ureq::Error::RedirectFailed
                    | ureq::Error::TooManyRedirects
                    | ureq::Error::InvalidProxyUrl
            ) {
                return false;
            }
            if matches!(e, ureq::Error::Tls(_)) {
                return true;
            }
            // rustls surfaces a rejected chain through several variants depending
            // on the transport (`Rustls`, or an `Io` wrapping it), and those are
            // documented as outside ureq's stable API — so classify on the
            // rendered message rather than on a variant that may be renamed.
            let msg = e.to_string().to_ascii_lowercase();
            ["certificate", "issuer", "handshake", "tls", "self-signed"]
                .iter()
                .any(|m| msg.contains(m))
        }

        /// Returns which trust path succeeded — `"bundled"` or `"platform"` —
        /// so the download state file can record it (see
        /// [`record_download_state_at`]); without that, "was this model fetched
        /// through the OS certificate chain?" is unanswerable after the fact.
        pub fn download_model_to(url: &str, dest_dir: &std::path::Path) -> Result<&'static str> {
            use std::io::Read as IoRead;

            tracing::info!("[model] Downloading model from {}...", url);

            // ureq's bundled webpki roots do not include a corporate MITM proxy's
            // private root, so on a TLS-inspecting network every fetch fails here
            // while `curl` (schannel / OS store) succeeds — the asymmetry issue
            // #35 reports. Try the bundled roots first (no behaviour change for
            // anyone it already worked for), then fall back to the OS trust
            // store — but only for failures a trust store can actually explain.
            let mut trust_path = "bundled";
            let mut response = match Self::agent_with_roots(false).get(url).call() {
                Ok(r) => r,
                Err(first) => {
                    if !Self::os_trust_store_might_help(&first) {
                        return Err(anyhow::anyhow!("Model download failed: {}", first));
                    }
                    tracing::warn!(
                        "[model-dl] Bundled-roots fetch failed ({}) — retrying with the OS certificate store",
                        first
                    );
                    trust_path = "platform";
                    Self::agent_with_roots(true).get(url).call().map_err(|e| {
                        anyhow::anyhow!(
                            "Model download failed (bundled roots: {}; OS trust store: {})",
                            first,
                            e
                        )
                    })?
                }
            };

            if response.status() != 200 {
                anyhow::bail!("Model download returned HTTP {}", response.status());
            }

            // Read body into memory (~83 MB for the published tarball; cap at 200MB)
            let mut body = Vec::new();
            response
                .body_mut()
                .as_reader()
                .take(200 * 1024 * 1024)
                .read_to_end(&mut body)?;

            // Extract + verify + atomically promote (see `extract_and_promote`).
            Self::extract_and_promote(&body, dest_dir, Self::MODEL_CONTENT_BLAKE3)?;

            tracing::info!(
                "[model] Model extracted and verified at {:?} ({} bytes, {} trust path)",
                dest_dir,
                body.len(),
                trust_path
            );
            Ok(trust_path)
        }

        /// Cheap fs-only probe: are model weights already on this machine at any
        /// of the resolution locations? Pure `exists()` checks — no weight
        /// loading — so hook-fast callers (CLI health-check) can distinguish
        /// "present but not loaded in this process" from "never downloaded".
        pub fn model_files_present() -> bool {
            Self::find_models_dir().is_ok()
        }

        /// What the on-disk weights will actually DO for this build — `"absent"`,
        /// `"unverified"`, or `"ready"`.
        ///
        /// `model_files_present` answers "is there a model.safetensors somewhere",
        /// which is the wrong question for the advice built on top of it. Files in
        /// the platform CACHE dir are additionally gated by the downloader's
        /// currency check (`cached_model_is_current`): weights that do not match
        /// this binary's pinned content are re-downloaded on the next server start,
        /// so telling an OFFLINE user who hand-filled that directory to "restart the
        /// MCP server" sends them through a restart that cannot help. Weights found
        /// anywhere else (CODE_GRAPH_MODEL_DIR, cwd/models, exe/models — the
        /// documented offline routes) are NOT gated: the loader takes them as they
        /// are, so they report `ready`.
        ///
        /// O(1) on purpose — marker plus companion `exists()` calls, never a hash.
        /// This runs on the health-check path, which hooks and the statusline call
        /// often; blake3 over ~80 MB of weights on every poll is not acceptable
        /// there, and the one place that legitimately hashes (the downloader) writes
        /// the marker that makes this check cheap forever after. A correct but
        /// not-yet-blessed cache therefore reads `unverified` until the next server
        /// start blesses it — pessimistic in exactly the direction that cannot
        /// mislead someone into a no-op restart.
        pub fn model_files_state() -> &'static str {
            let Ok(dir) = Self::find_models_dir() else {
                return "absent";
            };
            let is_cache = Self::cache_models_dir()
                .map(|cache| cache == dir)
                .unwrap_or(false);
            super::model_files_state_for(
                &dir,
                is_cache,
                Self::MODEL_CONTENT_BLAKE3,
                &Self::REQUIRED_COMPANION_FILES,
            )
        }

        fn find_models_dir() -> Result<std::path::PathBuf> {
            // 0. User-configured model directory (highest priority)
            if let Ok(custom_dir) = std::env::var("CODE_GRAPH_MODEL_DIR") {
                let custom = std::path::PathBuf::from(&custom_dir);
                if custom.join("model.safetensors").exists() {
                    tracing::info!(
                        "[model] Using custom model from CODE_GRAPH_MODEL_DIR={}",
                        custom_dir
                    );
                    return Ok(custom);
                }
                tracing::warn!(
                    "[model] CODE_GRAPH_MODEL_DIR={} set but model.safetensors not found there",
                    custom_dir
                );
            }

            // 1. Check relative to current working directory (dev environment)
            let cwd = std::env::current_dir()?;
            let models = cwd.join("models");
            if models.join("model.safetensors").exists() {
                return Ok(models);
            }

            // 2. Check relative to executable
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    let models = exe_dir.join("models");
                    if models.join("model.safetensors").exists() {
                        return Ok(models);
                    }
                }
            }

            // 3. Check CARGO_MANIFEST_DIR (for cargo test)
            if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
                let models = std::path::PathBuf::from(manifest).join("models");
                if models.join("model.safetensors").exists() {
                    return Ok(models);
                }
            }

            // 4. Check platform cache directory
            if let Ok(cache_dir) = Self::cache_models_dir() {
                if cache_dir.join("model.safetensors").exists() {
                    return Ok(cache_dir);
                }
            }

            anyhow::bail!("Model files not found. They will be downloaded on first use.")
        }

        /// Generate a 384-dim embedding for a single text.
        /// Uses masked mean pooling (consistent with embed_batch) to avoid
        /// diluting signal with padding zeros for texts shorter than 512 tokens.
        pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let encoding = self
                .tokenizer
                .encode(text, true)
                .map_err(|e| anyhow::anyhow!("tokenize error: {}", e))?;

            let ids = encoding.get_ids();
            let type_ids = encoding.get_type_ids();
            let seq_len = ids.len();

            let input_ids = Tensor::new(ids, &self.device)?.unsqueeze(0)?;
            let token_type_ids = Tensor::new(type_ids, &self.device)?.unsqueeze(0)?;
            // Build attention mask: 1.0 for real tokens
            let attention_vec: Vec<f32> = vec![1.0; seq_len];
            let attention_mask = Tensor::from_vec(attention_vec, (1, seq_len), &self.device)?;

            let embeddings =
                self.model
                    .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;

            // Masked mean pooling: only average over real tokens
            let attention_3d = attention_mask.unsqueeze(2)?;
            let masked = embeddings.broadcast_mul(&attention_3d)?;
            let summed = masked.sum(1)?;
            let count = attention_mask.sum(1)?.unsqueeze(1)?;
            let pooled = summed.broadcast_div(&count)?;
            let pooled = pooled.squeeze(0)?;

            let mut result: Vec<f32> = pooled.to_vec1()?;
            super::l2_normalize(&mut result);

            Ok(result)
        }

        /// Generate embeddings for multiple texts using true batched inference.
        /// Sorts texts by token length to minimize padding overhead, then batches.
        const BATCH_CHUNK: usize = 8;

        pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            if texts.len() == 1 {
                return Ok(vec![self.embed(texts[0])?]);
            }

            // Pre-tokenize to get lengths, sort by length to minimize padding
            let encodings: Vec<_> = texts
                .iter()
                .map(|t| {
                    self.tokenizer
                        .encode(*t, true)
                        .map_err(|e| anyhow::anyhow!("tokenize error: {}", e))
                })
                .collect::<Result<Vec<_>>>()?;

            // Build (original_index, encoding) pairs sorted by token count
            let mut indexed: Vec<(usize, _)> = encodings.into_iter().enumerate().collect();
            indexed.sort_by_key(|(_, enc)| enc.get_ids().len());

            // Process sorted chunks — sequences in each chunk have similar lengths
            let mut results_with_idx: Vec<(usize, Vec<f32>)> = Vec::with_capacity(texts.len());
            for chunk in indexed.chunks(Self::BATCH_CHUNK) {
                let chunk_encodings: Vec<&_> = chunk.iter().map(|(_, enc)| enc).collect();
                let chunk_results = self.embed_batch_chunk_pre_tokenized(&chunk_encodings)?;
                for (result, &(orig_idx, _)) in chunk_results.into_iter().zip(chunk.iter()) {
                    results_with_idx.push((orig_idx, result));
                }
            }

            // Restore original order
            results_with_idx.sort_by_key(|(idx, _)| *idx);
            Ok(results_with_idx.into_iter().map(|(_, v)| v).collect())
        }

        fn embed_batch_chunk_pre_tokenized(
            &self,
            encodings: &[&tokenizers::Encoding],
        ) -> Result<Vec<Vec<f32>>> {
            let max_len = encodings
                .iter()
                .map(|e| e.get_ids().len())
                .max()
                .unwrap_or(0);
            let batch_size = encodings.len();
            if max_len == 0 {
                // All encodings are empty — return zero vectors
                return Ok(vec![vec![0f32; super::EMBEDDING_DIM]; batch_size]);
            }

            // Build padded tensors
            let mut all_ids = vec![0u32; batch_size * max_len];
            let mut all_type_ids = vec![0u32; batch_size * max_len];
            let mut all_attention = vec![0f32; batch_size * max_len];

            for (i, enc) in encodings.iter().enumerate() {
                let ids = enc.get_ids();
                let type_ids = enc.get_type_ids();
                let seq_len = ids.len();
                let offset = i * max_len;
                all_ids[offset..offset + seq_len].copy_from_slice(ids);
                all_type_ids[offset..offset + seq_len].copy_from_slice(type_ids);
                for j in 0..seq_len {
                    all_attention[offset + j] = 1.0;
                }
            }

            let input_ids = Tensor::from_vec(all_ids, (batch_size, max_len), &self.device)?;
            let token_type_ids =
                Tensor::from_vec(all_type_ids, (batch_size, max_len), &self.device)?;
            let attention_mask =
                Tensor::from_vec(all_attention, (batch_size, max_len), &self.device)?;

            // Single forward pass for entire batch (pass attention mask for correct padding handling)
            let embeddings =
                self.model
                    .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;

            // Masked mean pooling per sequence
            let attention_3d = attention_mask.unsqueeze(2)?; // (batch, seq, 1)
            let masked = embeddings.broadcast_mul(&attention_3d)?;
            let summed = masked.sum(1)?; // (batch, hidden)
            let counts = attention_mask.sum(1)?.unsqueeze(1)?; // (batch, 1)
            let pooled = summed.broadcast_div(&counts)?;

            // Extract and normalize each vector
            let mut results = Vec::with_capacity(batch_size);
            for i in 0..batch_size {
                let mut vec: Vec<f32> = pooled.get(i)?.to_vec1()?;
                super::l2_normalize(&mut vec);
                results.push(vec);
            }
            Ok(results)
        }
    }
}

#[cfg(feature = "embed-model")]
pub use inner::EmbeddingModel;

#[cfg(not(feature = "embed-model"))]
pub struct EmbeddingModel;

#[cfg(not(feature = "embed-model"))]
impl EmbeddingModel {
    pub fn load() -> anyhow::Result<Option<Self>> {
        tracing::info!("Embedding model support not compiled (enable 'embed-model' feature), falling back to FTS5-only");
        Ok(None)
    }

    pub fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("Embedding model not compiled — enable the 'embed-model' feature")
    }

    #[allow(unused_variables)]
    pub fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("Embedding model not compiled — enable the 'embed-model' feature")
    }
}

#[cfg_attr(not(feature = "embed-model"), allow(dead_code))]
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Health-check advice used to key on "is there a model.safetensors", and
    /// answered "restart the MCP server" for weights the server will REPLACE.
    /// Hand-filling the platform cache is the offline user's move, and it is the
    /// one place a restart re-downloads instead of adopting — so the cache dir is
    /// the only location where a missing/stale `.model-id` downgrades the answer.
    #[test]
    fn model_files_state_separates_usable_weights_from_ones_that_get_replaced() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path();
        const ID: &str = "deadbeef";
        const COMPANIONS: [&str; 2] = ["tokenizer.json", "config.json"];

        assert_eq!(
            model_files_state_for(p, true, ID, &COMPANIONS),
            "absent",
            "no weights at all"
        );

        std::fs::write(p.join("model.safetensors"), b"w").unwrap();
        // Hand-placed in the CACHE dir: no marker, so the downloader will not
        // adopt these.
        assert_eq!(
            model_files_state_for(p, true, ID, &COMPANIONS),
            "unverified"
        );
        // The SAME directory reached through CODE_GRAPH_MODEL_DIR / cwd/models is
        // loaded as-is — this is the control that keeps the check about the cache
        // gate rather than about markers in general.
        assert_eq!(model_files_state_for(p, false, ID, &COMPANIONS), "ready");

        // A marker alone is not enough: the loader also needs the companions, and
        // verify_model_dir refuses to write the marker without them.
        std::fs::write(p.join(MODEL_ID_MARKER), ID).unwrap();
        assert_eq!(
            model_files_state_for(p, true, ID, &COMPANIONS),
            "unverified",
            "marker without companions must not read as ready"
        );
        for f in COMPANIONS {
            std::fs::write(p.join(f), b"{}").unwrap();
        }
        assert_eq!(model_files_state_for(p, true, ID, &COMPANIONS), "ready");

        // A marker from a DIFFERENT build is the stale-cache case the downloader
        // re-downloads for; it must not read as ready either.
        std::fs::write(p.join(MODEL_ID_MARKER), "some-older-release").unwrap();
        assert_eq!(
            model_files_state_for(p, true, ID, &COMPANIONS),
            "unverified"
        );
    }

    #[test]
    fn test_model_loads_gracefully() {
        // Should never panic, even if model files are missing
        let model = EmbeddingModel::load().unwrap();
        // Model availability depends on whether files exist
        if model.is_some() {
            println!("Model loaded successfully");
        } else {
            println!("Model not available (expected in CI without model files)");
        }
    }

    /// I1 (partial-download self-heal): a dir with correct weights but missing the
    /// loader's companion files must be REJECTED by verify (no marker) and reported
    /// not-current — never pinned as a usable cache. Adding the companions makes it
    /// verify and bless. Without this, a kill mid-extraction stranded the model FTS5-only.
    #[cfg(feature = "embed-model")]
    #[test]
    fn test_verify_model_dir_rejects_incomplete_then_accepts_complete() {
        use std::fs;
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("model.safetensors"), b"fake-weights-bytes").unwrap();
        let expected = blake3::hash(b"fake-weights-bytes").to_hex().to_string();

        // Weights match the pin but companions are missing → reject + don't bless.
        assert!(
            EmbeddingModel::verify_model_dir(dir.path(), &expected).is_err(),
            "incomplete dir (no tokenizer/config) must be rejected by verify"
        );
        assert!(
            !EmbeddingModel::cached_model_matches(dir.path(), &expected),
            "an incomplete dir must not be reported as the current cached model"
        );

        // Complete the dir → verify succeeds and the dir is now blessed.
        fs::write(dir.path().join("tokenizer.json"), b"{}").unwrap();
        fs::write(dir.path().join("config.json"), b"{}").unwrap();
        EmbeddingModel::verify_model_dir(dir.path(), &expected)
            .expect("a complete, hash-matching dir must verify");
        assert!(
            EmbeddingModel::cached_model_matches(dir.path(), &expected),
            "a complete, verified dir must be reported as current"
        );
    }

    /// The no-marker fallback in `cached_model_matches` (pre-marker caches / partial
    /// extractions) must likewise require companion files before writing the marker.
    #[cfg(feature = "embed-model")]
    #[test]
    fn test_cached_model_matches_no_marker_requires_companions() {
        use std::fs;
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("model.safetensors"), b"weights-v1").unwrap();
        let expected = blake3::hash(b"weights-v1").to_hex().to_string();

        // Correct weights, no marker, companions missing → must NOT bless.
        assert!(
            !EmbeddingModel::cached_model_matches(dir.path(), &expected),
            "weights-only dir (no marker, no companions) must not be treated as current"
        );

        // Add companions → fallback blesses and writes the marker; short-circuit then holds.
        fs::write(dir.path().join("tokenizer.json"), b"{}").unwrap();
        fs::write(dir.path().join("config.json"), b"{}").unwrap();
        assert!(
            EmbeddingModel::cached_model_matches(dir.path(), &expected),
            "complete dir must be treated as current"
        );
        assert!(
            EmbeddingModel::cached_model_matches(dir.path(), &expected),
            "marker short-circuit must also report current on the second call"
        );
    }

    /// I1 end-to-end: drive the real extract → verify → atomic-promote path with an
    /// in-memory model tar.gz. Covers (1) a complete archive promoting into a fresh cache,
    /// (2) an INCOMPLETE archive being rejected with the previous good cache left intact
    /// (self-heal: a partial download can't poison the cache), and (3) a new complete
    /// archive atomically replacing the old one — with no staging/old residue left behind.
    #[cfg(feature = "embed-model")]
    #[test]
    fn test_extract_and_promote_is_atomic_and_rejects_incomplete() {
        use inner::EmbeddingModel as M;
        use std::fs;

        fn put(b: &mut tar::Builder<flate2::write::GzEncoder<Vec<u8>>>, name: &str, data: &[u8]) {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, data).unwrap();
        }
        fn make_targz(weights: &[u8], with_companions: bool) -> Vec<u8> {
            let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut b = tar::Builder::new(gz);
            put(&mut b, "model.safetensors", weights);
            if with_companions {
                put(&mut b, "tokenizer.json", b"{}");
                put(&mut b, "config.json", b"{}");
            }
            b.into_inner().unwrap().finish().unwrap()
        }

        let root = tempfile::TempDir::new().unwrap();
        let dest = root.path().join("models");

        // (1) Complete archive promotes into a fresh dest.
        let w1 = b"fixture-weights-v1";
        let id1 = blake3::hash(w1).to_hex().to_string();
        M::extract_and_promote(&make_targz(w1, true), &dest, &id1).unwrap();
        assert!(dest.join("model.safetensors").exists());
        assert!(dest.join("tokenizer.json").exists() && dest.join("config.json").exists());
        assert!(
            M::cached_model_matches(&dest, &id1),
            "promoted dir must be current"
        );

        // (2) Incomplete archive (no companions) is rejected; the good v1 cache survives.
        let w2 = b"fixture-weights-v2";
        let id2 = blake3::hash(w2).to_hex().to_string();
        assert!(
            M::extract_and_promote(&make_targz(w2, false), &dest, &id2).is_err(),
            "an archive missing companion files must be rejected"
        );
        assert!(
            M::cached_model_matches(&dest, &id1),
            "a rejected partial download must leave the previous good cache intact"
        );

        // (3) New complete archive atomically replaces the old one.
        M::extract_and_promote(&make_targz(w2, true), &dest, &id2).unwrap();
        assert!(
            M::cached_model_matches(&dest, &id2),
            "new model must replace old"
        );
        assert!(
            !M::cached_model_matches(&dest, &id1),
            "old model id must no longer match"
        );

        // No staging/old residue — only the live cache dir remains in the parent.
        let mut names: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["models".to_string()],
            "staging/old dirs must be cleaned up, found: {:?}",
            names
        );
    }

    /// The unpack budget is enforced from the tar HEADER, before any member is
    /// written.
    ///
    /// Extraction runs before the blake3 pin can be checked (the pin is over the
    /// unpacked weights), and the only size cap was on the COMPRESSED body — so
    /// a bundle whose gzip ratio is hostile could write gigabytes of unverified
    /// data to the user's disk before anything rejected it.
    #[cfg(feature = "embed-model")]
    #[test]
    fn test_extract_refuses_an_archive_that_unpacks_past_the_budget() {
        use inner::EmbeddingModel as M;

        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut b = tar::Builder::new(gz);
        let payload = vec![b'0'; 4096]; // compresses to nearly nothing
        let mut h = tar::Header::new_gnu();
        h.set_size(payload.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "model.safetensors", &payload[..])
            .unwrap();
        let body = b.into_inner().unwrap().finish().unwrap();
        assert!(
            body.len() < payload.len(),
            "fixture must actually compress, else it proves nothing about ratios"
        );

        let root = tempfile::TempDir::new().unwrap();
        let dest = root.path().join("models");
        let id = blake3::hash(&payload).to_hex().to_string();

        let err = M::extract_and_promote_capped(&body, &dest, &id, 1024)
            .expect_err("an archive declaring more than the budget must be refused");
        assert!(
            err.to_string().contains("refusing to fill the disk"),
            "the refusal must name its reason, got: {err}"
        );
        assert!(
            !dest.exists(),
            "nothing may be promoted from a refused archive"
        );

        // Same archive under a budget that fits gets past the guard — it is a
        // bound, not a blanket refusal. (It still fails, on the missing
        // companion files, which is a different rejection.)
        let err = M::extract_and_promote_capped(&body, &dest, &id, 64 * 1024)
            .expect_err("fixture has no tokenizer.json/config.json");
        assert!(
            !err.to_string().contains("refusing to fill the disk"),
            "under a sufficient budget the size guard must not be what rejects: {err}"
        );
    }

    /// The OS-trust-store retry must fire only for failures a different root
    /// store could explain. Retrying a timeout or a 404 costs a second full
    /// 600s budget for nothing, and with the caller's three attempts that is
    /// close to an hour of `doctor` reporting "download in flight".
    #[cfg(feature = "embed-model")]
    #[test]
    fn test_os_trust_store_retry_is_limited_to_trust_failures() {
        use inner::EmbeddingModel as M;
        assert!(
            M::os_trust_store_might_help(&ureq::Error::Tls("invalid peer certificate")),
            "a TLS error is exactly what the OS store can fix"
        );
        assert!(
            !M::os_trust_store_might_help(&ureq::Error::StatusCode(404)),
            "a 404 is the same 404 under any root store"
        );
        assert!(
            !M::os_trust_store_might_help(&ureq::Error::HostNotFound),
            "DNS is not a trust decision"
        );
        assert!(
            !M::os_trust_store_might_help(&ureq::Error::TooManyRedirects),
            "redirect loops do not depend on the root store"
        );
    }

    /// The trust path that produced the bytes is recorded, so `doctor` can
    /// answer "did this model come through the OS certificate chain?".
    #[cfg(feature = "embed-model")]
    #[test]
    fn test_download_state_records_the_trust_path() {
        use inner::EmbeddingModel as M;
        let tmp = tempfile::TempDir::new().unwrap();
        let state = tmp.path().join("model-download.json");

        M::record_download_state_at_trusted(&state, "ok", 1, None, Some("platform"));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        assert_eq!(v["trust_path"], "platform");

        // Absent rather than fabricated when the caller does not know.
        M::record_download_state_at(&state, "in_flight", 1, None);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        assert!(
            v["trust_path"].is_null(),
            "an unknown trust path must read as unknown, not as bundled"
        );
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_embed_produces_correct_dims() {
        let model = EmbeddingModel::load().unwrap();
        // This is the CHEAP real-weight check, and the release gate is what runs
        // it: do the weights we are about to publish load and produce a usable
        // vector at all? Without this assert it passes by doing nothing whenever
        // no model is cached — the same silent no-op that let the whole pipeline
        // reach `npm publish` with zero real-weight coverage (audit 2026-09-05
        // ENG-02).
        //
        // The thorough half — the two backfill regressions in
        // tests/mcp_stdio_integration.rs — is deliberately NOT on the release
        // path: they fail intermittently (2 runs in 6 against real weights,
        // `0 vectors after 300 s`, passing in 51 s alone) for a reason nobody has
        // found yet. Not slowness: the budget is per test and each finished well
        // inside it even pinned to 2 cores. An unexplained 2-in-6 in front of an
        // irreversible publish is a coin flip, so those two run on the cache-warm
        // cron instead (pre-ship review 2026-09-06).
        assert!(
            model.is_some() || std::env::var_os("CODE_GRAPH_REQUIRE_MODEL").is_none(),
            "CODE_GRAPH_REQUIRE_MODEL=1 is set but the embedding model did not \
             load — this check would have passed by doing nothing, which is the \
             state that flag exists to make impossible"
        );
        if let Some(model) = model {
            let embedding = model
                .embed("function validateToken handles JWT auth")
                .unwrap();
            assert_eq!(embedding.len(), EMBEDDING_DIM);

            // Verify L2 normalization: ||v|| ≈ 1.0
            let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!(
                (norm - 1.0).abs() < 0.01,
                "norm should be ~1.0, got {}",
                norm
            );
        }
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_embed_batch() {
        let model = EmbeddingModel::load().unwrap();
        if let Some(model) = model {
            let texts = vec!["function foo", "class Bar", "route /api/login"];
            let embeddings = model.embed_batch(&texts).unwrap();
            assert_eq!(embeddings.len(), 3);
            for emb in &embeddings {
                assert_eq!(emb.len(), EMBEDDING_DIM);
            }
        }
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_embed_batch_matches_sequential() {
        let model = EmbeddingModel::load().unwrap();
        if let Some(model) = model {
            let texts = vec![
                "function validateToken JWT authentication",
                "short",
                "function handleLogin with a much longer context string that tests padding behavior in batched inference",
            ];
            // Get sequential results
            let sequential: Vec<Vec<f32>> = texts.iter().map(|t| model.embed(t).unwrap()).collect();
            // Get batched results
            let batched = model.embed_batch(&texts).unwrap();

            assert_eq!(sequential.len(), batched.len());
            for (i, (seq, bat)) in sequential.iter().zip(batched.iter()).enumerate() {
                let sim = cosine_sim(seq, bat);
                assert!(
                    sim > 0.99,
                    "batch vs sequential similarity for text {}: {} (should be >0.99)",
                    i,
                    sim
                );
            }
        }
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_similar_texts_closer() {
        let model = EmbeddingModel::load().unwrap();
        if let Some(model) = model {
            let auth = model
                .embed("function validateToken JWT authentication")
                .unwrap();
            let login = model
                .embed("function handleLogin user authentication")
                .unwrap();
            let sort = model
                .embed("function bubbleSort array sorting algorithm")
                .unwrap();

            let sim_auth_login = cosine_sim(&auth, &login);
            let sim_auth_sort = cosine_sim(&auth, &sort);

            assert!(
                sim_auth_login > sim_auth_sort,
                "auth-login similarity ({}) should be > auth-sort similarity ({})",
                sim_auth_login,
                sim_auth_sort
            );
        }
    }

    #[cfg(feature = "embed-model")]
    fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn test_l2_normalize() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_l2_normalize_zero_vector() {
        let mut v = vec![0.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_cache_dir_resolves() {
        let dir = inner::EmbeddingModel::cache_models_dir();
        assert!(dir.is_ok(), "cache dir should resolve: {:?}", dir);
        let dir = dir.unwrap();
        assert!(
            dir.to_str().unwrap().contains("code-graph"),
            "cache dir should contain 'code-graph': {:?}",
            dir
        );
    }

    /// Issue #35: a download that silently no-ops left the user permanently
    /// FTS5-only with nothing to act on, and `doctor` printed the same
    /// "retry shortly" forever. Each recorded status must round-trip into a
    /// summary that names what actually happened — in particular "failed" must
    /// carry the error and be distinguishable from "never attempted" (`None`).
    #[cfg(feature = "embed-model")]
    #[test]
    fn test_download_state_round_trips_each_status() {
        use inner::EmbeddingModel as M;
        let tmp = tempfile::TempDir::new().unwrap();
        // Inject the state-file path. The predecessor of this test redirected
        // `dirs::cache_dir()` via XDG_CACHE_HOME/LOCALAPPDATA, which is a no-op on
        // macOS and Windows (see `record_download_state_at`): there it wrote the
        // developer's real model-download.json and then failed its own first
        // assertion on the next run. The temp path also makes the test
        // order-independent and drops the `env::set_var` race with the sibling
        // tests that read `dirs::cache_dir()` on the same thread pool.
        let state = tmp.path().join("model-download.json");

        assert!(
            M::download_state_summary_at(&state).is_none(),
            "no recorded attempt must read as 'never attempted', not as a status"
        );

        M::record_download_state_at(&state, "in_flight", 2, None);
        assert_eq!(
            M::download_state_summary_at(&state).unwrap(),
            "download in flight (attempt 2)"
        );

        M::record_download_state_at(&state, "failed", 3, Some("tls handshake rejected"));
        let summary = M::download_state_summary_at(&state).unwrap();
        assert!(
            summary.contains("FAILED after 3 attempt(s)")
                && summary.contains("tls handshake rejected"),
            "a failed download must surface attempt count AND cause, got: {summary}"
        );

        M::record_download_state_at(&state, "ok", 1, None);
        assert_eq!(
            M::download_state_summary_at(&state).unwrap(),
            "model downloaded successfully"
        );
    }

    /// The production wrappers must still route through the real cache location —
    /// otherwise the seam above could pass while `doctor` reads a different file.
    #[cfg(feature = "embed-model")]
    #[test]
    fn test_download_state_file_sits_beside_models_dir() {
        use inner::EmbeddingModel as M;
        let models = M::cache_models_dir().unwrap();
        let state = M::download_state_file().unwrap();
        assert_eq!(
            state.parent(),
            models.parent(),
            "state file must sit beside the models dir, not inside it \
             (extract_and_promote renames the models dir)"
        );
        assert_eq!(state.file_name().unwrap(), "model-download.json");
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_download_model_invalid_url_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = inner::EmbeddingModel::download_model_to(
            "https://invalid.example.com/nonexistent.tar.gz",
            tmp.path(),
        );
        let err = result.expect_err("should fail on invalid URL").to_string();
        // …and fail ONCE. A name that does not resolve (or a connection that is
        // refused) is not something the OS certificate store can fix, so the
        // second full 600s attempt must not be spent — the two-attempt message
        // shape is what betrays it if the narrowing regresses.
        assert!(
            !err.contains("OS trust store"),
            "a non-trust failure must not trigger the trust-store retry: {err}"
        );
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_model_download_url_contains_version() {
        let url = inner::EmbeddingModel::model_download_url();
        assert!(
            url.contains(env!("CARGO_PKG_VERSION")),
            "URL should contain package version: {}",
            url
        );
        assert!(
            url.contains("models.tar.gz"),
            "URL should point to models.tar.gz: {}",
            url
        );
    }

    // ── cached_model_matches / verify_model_dir (model-pin guard) ──────────

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_cached_model_matches_is_identity_aware_not_existence_only() {
        use inner::EmbeddingModel as M;
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let content = b"fake-weights";
        let expected = blake3::hash(content).to_hex().to_string();

        // Missing weights → not current.
        assert!(!M::cached_model_matches(dir, &expected));

        // Weights present but companions missing (partial extraction) → NOT current,
        // and no marker written — the dir is unloadable until completed.
        std::fs::write(dir.join("model.safetensors"), content).unwrap();
        assert!(
            !M::cached_model_matches(dir, &expected),
            "weights-only dir without tokenizer/config must not be blessed"
        );
        assert!(
            !dir.join(".model-id").exists(),
            "an incomplete dir must not get a blessing marker"
        );

        // Pre-v0.50 cache (no marker) that is COMPLETE: matching content → current,
        // marker written by the one-time hash fallback.
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        std::fs::write(dir.join("config.json"), b"{}").unwrap();
        assert!(M::cached_model_matches(dir, &expected));
        assert_eq!(
            std::fs::read_to_string(dir.join(".model-id"))
                .unwrap()
                .trim(),
            expected,
            "one-time hash must persist the marker"
        );

        // Marker present and matching → current (O(1) path).
        assert!(M::cached_model_matches(dir, &expected));

        // Binary now expects different weights (new pinned model) → stale.
        let other = blake3::hash(b"new-model").to_hex().to_string();
        assert!(
            !M::cached_model_matches(dir, &other),
            "stale cached model must be reported as not current"
        );
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_model_content_pin_matches_local_weights() {
        // A typo in MODEL_CONTENT_BLAKE3 would make every client reject the
        // released models.tar.gz forever. When the repo's models/ dir holds
        // the pinned weights (dev machines; CI release runners don't), assert
        // the constant matches the real file. Skips silently otherwise.
        let local = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("models")
            .join("model.safetensors");
        if !local.exists() {
            return;
        }
        let dir = local.parent().unwrap();
        assert!(
            inner::EmbeddingModel::cached_model_matches(
                dir,
                inner::EmbeddingModel::MODEL_CONTENT_BLAKE3
            ),
            "MODEL_CONTENT_BLAKE3 does not match models/model.safetensors — \
             constant typo or local models/ dir drifted from the pinned HF revision"
        );
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_verify_model_dir_rejects_and_removes_mismatched_weights() {
        use inner::EmbeddingModel as M;
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("model.safetensors"), b"tampered").unwrap();

        let expected = blake3::hash(b"genuine").to_hex().to_string();
        let result = M::verify_model_dir(dir, &expected);
        assert!(result.is_err(), "mismatched weights must be rejected");
        assert!(
            !dir.join("model.safetensors").exists(),
            "rejected weights must be removed so a poisoned cache can't load later"
        );

        // Matching content in a COMPLETE dir verifies and writes the marker.
        std::fs::write(dir.join("model.safetensors"), b"genuine").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        std::fs::write(dir.join("config.json"), b"{}").unwrap();
        M::verify_model_dir(dir, &expected).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".model-id"))
                .unwrap()
                .trim(),
            expected
        );
    }
}
