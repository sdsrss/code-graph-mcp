use anyhow::Result;

pub const EMBEDDING_DIM: usize = 384;

pub struct EmbeddingModel {
    available: bool,
}

impl EmbeddingModel {
    /// Try to load the embedding model. Returns a model that gracefully
    /// degrades to unavailable if model files are not present.
    pub fn load() -> Result<Self> {
        // TODO: Load candle model from models/ directory or include_bytes!
        // For now, return unavailable — system falls back to FTS5-only search
        Ok(Self { available: false })
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    pub fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        if !self.available {
            anyhow::bail!("Embedding model not available");
        }
        // TODO: Implement candle inference
        Ok(vec![0.0; EMBEDDING_DIM])
    }

    pub fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if !self.available {
            anyhow::bail!("Embedding model not available");
        }
        // TODO: Implement candle batch inference
        Ok(vec![vec![0.0; EMBEDDING_DIM]; _texts.len()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_loads_gracefully() {
        let model = EmbeddingModel::load().unwrap();
        // Model should load without error even without model files
        assert!(!model.is_available());
    }

    #[test]
    fn test_embed_returns_error_when_unavailable() {
        let model = EmbeddingModel::load().unwrap();
        assert!(model.embed("test").is_err());
    }
}
