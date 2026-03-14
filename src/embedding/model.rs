// Re-export from domain (canonical source)
pub use crate::domain::EMBEDDING_DIM;

#[cfg(feature = "embed-model")]
mod inner {
    use anyhow::Result;
    use candle_core::{Device, DType, Tensor};
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
                    tracing::warn!("Embedding model not available, falling back to FTS5-only: {}", e);
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

            Ok(Self { model, tokenizer, device })
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

        /// URL for model download, based on current binary version.
        pub fn model_download_url() -> String {
            let version = env!("CARGO_PKG_VERSION");
            format!(
                "https://github.com/sdsrss/code-graph-mcp/releases/download/v{}/models.tar.gz",
                version
            )
        }

        /// Download model tarball from URL with timeout, extract to dest_dir.
        /// Integrity: HTTPS transport + valid tar.gz extraction + blake3 version marker.
        pub fn download_model_to(url: &str, dest_dir: &std::path::Path) -> Result<()> {
            use std::io::Read as IoRead;

            tracing::info!("[model] Downloading model from {}...", url);

            let agent = ureq::Agent::new_with_config(
                ureq::config::Config::builder()
                    .timeout_global(Some(std::time::Duration::from_secs(120)))
                    .build()
            );

            let mut response = agent.get(url)
                .call()
                .map_err(|e| anyhow::anyhow!("Model download failed: {}", e))?;

            if response.status() != 200 {
                anyhow::bail!("Model download returned HTTP {}", response.status());
            }

            // Read body into memory (model is ~30MB compressed, cap at 200MB)
            let mut body = Vec::new();
            response.body_mut().as_reader()
                .take(200 * 1024 * 1024)
                .read_to_end(&mut body)?;

            // Extract tar.gz (also validates integrity — corrupt archives fail here)
            std::fs::create_dir_all(dest_dir)?;
            let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(&body));
            let mut archive = tar::Archive::new(gz);
            archive.unpack(dest_dir)?;

            // Write version marker (blake3 hash of tarball for cache invalidation)
            let hash = blake3::hash(&body);
            std::fs::write(dest_dir.join(".version"), hash.to_hex().as_str())?;

            tracing::info!("[model] Model extracted to {:?} ({} bytes)", dest_dir, body.len());
            Ok(())
        }

        fn find_models_dir() -> Result<std::path::PathBuf> {
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
        pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
            let encoding = self.tokenizer.encode(text, true)
                .map_err(|e| anyhow::anyhow!("tokenize error: {}", e))?;

            let ids = encoding.get_ids();
            let type_ids = encoding.get_type_ids();

            let input_ids = Tensor::new(ids, &self.device)?.unsqueeze(0)?;
            let token_type_ids = Tensor::new(type_ids, &self.device)?.unsqueeze(0)?;

            let embeddings = self.model.forward(&input_ids, &token_type_ids, None)?;

            // Mean pooling: average across token dimension
            let (_batch, n_tokens, _hidden) = embeddings.dims3()?;
            let pooled = (embeddings.sum(1)? / (n_tokens as f64))?;
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
            let encodings: Vec<_> = texts.iter()
                .map(|t| self.tokenizer.encode(*t, true)
                    .map_err(|e| anyhow::anyhow!("tokenize error: {}", e)))
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

        fn embed_batch_chunk_pre_tokenized(&self, encodings: &[&tokenizers::Encoding]) -> Result<Vec<Vec<f32>>> {
            let max_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
            let batch_size = encodings.len();

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
            let token_type_ids = Tensor::from_vec(all_type_ids, (batch_size, max_len), &self.device)?;
            let attention_mask = Tensor::from_vec(all_attention, (batch_size, max_len), &self.device)?;

            // Single forward pass for entire batch (pass attention mask for correct padding handling)
            let embeddings = self.model.forward(&input_ids, &token_type_ids, Some(&attention_mask))?;

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

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_embed_produces_correct_dims() {
        let model = EmbeddingModel::load().unwrap();
        if let Some(model) = model {
            let embedding = model.embed("function validateToken handles JWT auth").unwrap();
            assert_eq!(embedding.len(), EMBEDDING_DIM);

            // Verify L2 normalization: ||v|| ≈ 1.0
            let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 0.01, "norm should be ~1.0, got {}", norm);
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
            let sequential: Vec<Vec<f32>> = texts.iter()
                .map(|t| model.embed(t).unwrap())
                .collect();
            // Get batched results
            let batched = model.embed_batch(&texts).unwrap();

            assert_eq!(sequential.len(), batched.len());
            for (i, (seq, bat)) in sequential.iter().zip(batched.iter()).enumerate() {
                let sim = cosine_sim(seq, bat);
                assert!(sim > 0.99, "batch vs sequential similarity for text {}: {} (should be >0.99)", i, sim);
            }
        }
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_similar_texts_closer() {
        let model = EmbeddingModel::load().unwrap();
        if let Some(model) = model {
            let auth = model.embed("function validateToken JWT authentication").unwrap();
            let login = model.embed("function handleLogin user authentication").unwrap();
            let sort = model.embed("function bubbleSort array sorting algorithm").unwrap();

            let sim_auth_login = cosine_sim(&auth, &login);
            let sim_auth_sort = cosine_sim(&auth, &sort);

            assert!(
                sim_auth_login > sim_auth_sort,
                "auth-login similarity ({}) should be > auth-sort similarity ({})",
                sim_auth_login, sim_auth_sort
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
        assert!(dir.to_str().unwrap().contains("code-graph"),
            "cache dir should contain 'code-graph': {:?}", dir);
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_download_model_invalid_url_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = inner::EmbeddingModel::download_model_to(
            "https://invalid.example.com/nonexistent.tar.gz",
            tmp.path(),
        );
        assert!(result.is_err(), "should fail on invalid URL");
    }

    #[cfg(feature = "embed-model")]
    #[test]
    fn test_model_download_url_contains_version() {
        let url = inner::EmbeddingModel::model_download_url();
        assert!(url.contains(env!("CARGO_PKG_VERSION")),
            "URL should contain package version: {}", url);
        assert!(url.contains("models.tar.gz"),
            "URL should point to models.tar.gz: {}", url);
    }
}
