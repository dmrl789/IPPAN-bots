use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResult {
    pub accepted: u64,
    pub rejected: u64,
    pub timeouts: u64,
    pub latency_ms: u64,
}

/// Trait for transaction submission adapters
pub trait TxSubmitter: Send + Sync {
    fn name(&self) -> &'static str;

    fn submit_batch<'a>(
        &'a self,
        batch: &'a [Vec<u8>],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<SubmitResult>> + Send + 'a>>;
}

/// Mock submitter for testing (always accepts with configurable delay)
pub struct MockSubmitter {
    delay_ms: u64,
}

impl MockSubmitter {
    pub fn new(delay_ms: u64) -> Self {
        Self { delay_ms }
    }
}

impl TxSubmitter for MockSubmitter {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn submit_batch<'a>(
        &'a self,
        batch: &'a [Vec<u8>],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<SubmitResult>> + Send + 'a>> {
        Box::pin(async move {
            sleep(Duration::from_millis(self.delay_ms)).await;

            Ok(SubmitResult {
                accepted: batch.len() as u64,
                rejected: 0,
                timeouts: 0,
                latency_ms: self.delay_ms,
            })
        })
    }
}

/// HTTP/JSON submitter for IPPAN RPC (currently stubbed)
pub struct HttpJsonSubmitter {
    client: reqwest::Client,
    urls: Vec<String>,
    current_url_idx: std::sync::atomic::AtomicUsize,
    timeout: Duration,
}

impl HttpJsonSubmitter {
    pub fn new(urls: Vec<String>, timeout_ms: u64) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()?;

        Ok(Self {
            client,
            urls,
            current_url_idx: std::sync::atomic::AtomicUsize::new(0),
            timeout: Duration::from_millis(timeout_ms),
        })
    }

    fn next_url(&self) -> &str {
        let idx = self
            .current_url_idx
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        &self.urls[idx % self.urls.len()]
    }
}

impl TxSubmitter for HttpJsonSubmitter {
    fn name(&self) -> &'static str {
        "http"
    }

    fn submit_batch<'a>(
        &'a self,
        batch: &'a [Vec<u8>],
    ) -> Pin<Box<dyn std::future::Future<Output = Result<SubmitResult>> + Send + 'a>> {
        Box::pin(async move {
            let start = std::time::Instant::now();
            let url = self.next_url();

            // TODO: Align with IPPAN RPC specification
            // This is a stubbed implementation with placeholder request format
            let request_body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "submit_transaction_batch",
                "params": {
                    "transactions": batch.iter()
                        .map(|tx| hex::encode(tx))
                        .collect::<Vec<_>>()
                }
            });

            // TODO: Replace with actual IPPAN endpoint path
            let endpoint = format!("{}/rpc", url);

            match self.client.post(&endpoint).json(&request_body).send().await {
                Ok(response) => {
                    let latency_ms = start.elapsed().as_millis() as u64;

                    if response.status().is_success() {
                        // TODO: Parse actual IPPAN response format
                        // For now, assume all accepted
                        Ok(SubmitResult {
                            accepted: batch.len() as u64,
                            rejected: 0,
                            timeouts: 0,
                            latency_ms,
                        })
                    } else {
                        // TODO: Parse error response for rejected count
                        Ok(SubmitResult {
                            accepted: 0,
                            rejected: batch.len() as u64,
                            timeouts: 0,
                            latency_ms,
                        })
                    }
                }
                Err(e) if e.is_timeout() => Ok(SubmitResult {
                    accepted: 0,
                    rejected: 0,
                    timeouts: batch.len() as u64,
                    latency_ms: self.timeout.as_millis() as u64,
                }),
                Err(e) => Err(e.into()),
            }
        })
    }
}

// Add hex encoding support for HttpJsonSubmitter
mod hex {
    pub fn encode(data: &[u8]) -> String {
        data.iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    }
}
