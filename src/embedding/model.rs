pub const EMBEDDING_DIM: usize = 384;

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

            // Truncate to 128 tokens — sufficient for code context strings
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: 128,
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

        fn find_models_dir() -> Result<std::path::PathBuf> {
            // Check relative to current working directory (dev environment)
            let cwd = std::env::current_dir()?;
            let models = cwd.join("models");
            if models.join("model.safetensors").exists() {
                return Ok(models);
            }

            // Check relative to executable
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    let models = exe_dir.join("models");
                    if models.join("model.safetensors").exists() {
                        return Ok(models);
                    }
                }
            }

            // Check CARGO_MANIFEST_DIR (for cargo test)
            if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
                let models = std::path::PathBuf::from(manifest).join("models");
                if models.join("model.safetensors").exists() {
                    return Ok(models);
                }
            }

            anyhow::bail!("Model files not found in models/ directory")
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

        /// Generate embeddings for multiple texts (sequential, no padding needed).
        pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            texts.iter().map(|t| self.embed(t)).collect()
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

    pub fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
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
}
