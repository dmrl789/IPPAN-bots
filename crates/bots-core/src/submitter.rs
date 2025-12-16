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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentTx {
    pub from: String,
    pub to: String,
    pub amount: String,
    pub signing_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
}

/// Trait for transaction submission adapters
pub trait TxSubmitter: Send + Sync {
    fn name(&self) -> &'static str;

    fn submit_payment<'a>(
        &'a self,
        tx: &'a PaymentTx,
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

    fn submit_payment<'a>(
        &'a self,
        _tx: &'a PaymentTx,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<SubmitResult>> + Send + 'a>> {
        Box::pin(async move {
            sleep(Duration::from_millis(self.delay_ms)).await;

            Ok(SubmitResult {
                accepted: 1,
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

    fn submit_payment<'a>(
        &'a self,
        tx: &'a PaymentTx,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<SubmitResult>> + Send + 'a>> {
        Box::pin(async move {
            let start = std::time::Instant::now();
            let url = self.next_url();

            // POST to /tx/payment endpoint with single transaction
            let endpoint = format!("{}/tx/payment", url);

            match self.client.post(&endpoint).json(&tx).send().await {
                Ok(response) => {
                    let latency_ms = start.elapsed().as_millis() as u64;

                    if response.status().is_success() {
                        // Try to parse response
                        match response.json::<serde_json::Value>().await {
                            Ok(body) => {
                                // Success if response has tx_hash field or is 2xx
                                if body.get("tx_hash").is_some() {
                                    Ok(SubmitResult {
                                        accepted: 1,
                                        rejected: 0,
                                        timeouts: 0,
                                        latency_ms,
                                    })
                                } else if body.get("code").is_some() || body.get("error").is_some()
                                {
                                    // Error response
                                    Ok(SubmitResult {
                                        accepted: 0,
                                        rejected: 1,
                                        timeouts: 0,
                                        latency_ms,
                                    })
                                } else {
                                    // Assume success if 2xx
                                    Ok(SubmitResult {
                                        accepted: 1,
                                        rejected: 0,
                                        timeouts: 0,
                                        latency_ms,
                                    })
                                }
                            }
                            Err(_) => {
                                // If can't parse JSON, assume success on 2xx
                                Ok(SubmitResult {
                                    accepted: 1,
                                    rejected: 0,
                                    timeouts: 0,
                                    latency_ms,
                                })
                            }
                        }
                    } else {
                        // Non-2xx status = rejected
                        Ok(SubmitResult {
                            accepted: 0,
                            rejected: 1,
                            timeouts: 0,
                            latency_ms,
                        })
                    }
                }
                Err(e) if e.is_timeout() => Ok(SubmitResult {
                    accepted: 0,
                    rejected: 0,
                    timeouts: 1,
                    latency_ms: self.timeout.as_millis() as u64,
                }),
                Err(e) => Err(e.into()),
            }
        })
    }
}
