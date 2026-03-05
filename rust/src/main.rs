//! sheets-on-chain backend
//!
//! Exposes two interfaces to the Google Sheets sidebar:
//!
//! 1. **HTTP POST `/`** — standard JSON-RPC 2.0 batch endpoint.
//!    The sidebar sends an array of `eth_*` calls; this server
//!    forwards them to the upstream RPC node and returns the batch
//!    response array.
//!
//! 2. **WebSocket `ws://host/`** — JSON-RPC 2.0 over WebSocket.
//!    The sidebar sends `eth_subscribe` / `newHeads` and receives
//!    `eth_subscription` notifications on every new block, with the
//!    same shape as a standard Ethereum node subscription.

use anyhow::Result;
use clap::Parser;
use jsonrpsee::server::{RpcModule, Server, SubscriptionMessage};
use jsonrpsee::types::ErrorObjectOwned;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

// ── CLI args ──────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "sheets-on-chain-backend", about = "Ethereum JSON-RPC proxy for Google Sheets")]
struct Args {
    /// Ethereum node HTTP/WS RPC URL (e.g. wss://mainnet.infura.io/ws/v3/KEY)
    #[arg(long, env = "ETH_RPC_URL")]
    rpc_url: String,

    /// Address to listen on
    #[arg(long, env = "LISTEN_ADDR", default_value = "127.0.0.1:3000")]
    listen: SocketAddr,
}

// ── Shared state ──────────────────────────────────────────────────────────────

struct AppState {
    /// Upstream Ethereum node HTTP URL (used for batch calls).
    upstream_http: String,
    /// Broadcast channel – sends new block headers to all WS subscribers.
    new_heads_tx: broadcast::Sender<Value>,
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    info!("Upstream RPC: {}", args.rpc_url);
    info!("Listening on: {}", args.listen);

    let (new_heads_tx, _) = broadcast::channel::<Value>(64);

    let state = Arc::new(AppState {
        upstream_http: to_http_url(&args.rpc_url),
        new_heads_tx: new_heads_tx.clone(),
    });

    // Spawn the newHeads watcher in the background.
    let ws_rpc_url = args.rpc_url.clone();
    let tx = new_heads_tx.clone();
    tokio::spawn(async move {
        watch_new_heads(&ws_rpc_url, tx).await;
    });

    // Build and start the jsonrpsee server.
    let server = Server::builder().build(args.listen).await?;
    let module = build_rpc_module(state);
    let handle = server.start(module);
    info!("Server started");
    handle.stopped().await;
    Ok(())
}

/// Converts ws(s):// → http(s):// for use with reqwest.
fn to_http_url(url: &str) -> String {
    url.replace("wss://", "https://")
       .replace("ws://", "http://")
}

// ── RPC module ────────────────────────────────────────────────────────────────

fn build_rpc_module(state: Arc<AppState>) -> RpcModule<Arc<AppState>> {
    let mut module = RpcModule::new(state);

    // ── eth_getBalance ────────────────────────────────────────────────────────
    module
        .register_async_method("eth_getBalance", |params, ctx, _ext| async move {
            let (address, block): (String, String) = params.parse()?;
            proxy_call(&ctx.upstream_http, "eth_getBalance", serde_json::json!([address, block]))
                .await
                .map_err(rpc_err)
        })
        .unwrap();

    // ── eth_call ──────────────────────────────────────────────────────────────
    module
        .register_async_method("eth_call", |params, ctx, _ext| async move {
            let (tx_obj, block): (Value, String) = params.parse()?;
            proxy_call(&ctx.upstream_http, "eth_call", serde_json::json!([tx_obj, block]))
                .await
                .map_err(rpc_err)
        })
        .unwrap();

    // ── eth_blockNumber ───────────────────────────────────────────────────────
    module
        .register_async_method("eth_blockNumber", |_params, ctx, _ext| async move {
            proxy_call(&ctx.upstream_http, "eth_blockNumber", serde_json::json!([]))
                .await
                .map_err(rpc_err)
        })
        .unwrap();

    // ── eth_getBlockByNumber ──────────────────────────────────────────────────
    module
        .register_async_method("eth_getBlockByNumber", |params, ctx, _ext| async move {
            let (block, full): (String, bool) = params.parse()?;
            proxy_call(
                &ctx.upstream_http,
                "eth_getBlockByNumber",
                serde_json::json!([block, full]),
            )
            .await
            .map_err(rpc_err)
        })
        .unwrap();

    // ── eth_subscribe (newHeads) ──────────────────────────────────────────────
    //
    // Returns a subscription ID; new block headers are pushed via
    // `eth_subscription` notifications over the same WebSocket connection.
    module
        .register_subscription(
            "eth_subscribe",
            "eth_subscription",
            "eth_unsubscribe",
            |params, pending, ctx, _ext| async move {
                let kind: String = match params.one::<String>() {
                    Ok(k) => k,
                    Err(_) => {
                        pending.reject(ErrorObjectOwned::owned(
                            -32602,
                            "Expected subscription kind string",
                            None::<()>,
                        )).await;
                        return Ok(());
                    }
                };
                if kind != "newHeads" {
                    pending.reject(ErrorObjectOwned::owned(
                        -32601,
                        format!("Unsupported subscription: {kind}"),
                        None::<()>,
                    )).await;
                    return Ok(());
                }

                let sink = pending.accept().await?;
                let mut rx = ctx.new_heads_tx.subscribe();
                tokio::spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(header) => {
                                let msg = match SubscriptionMessage::from_json(&header) {
                                    Ok(m) => m,
                                    Err(_) => break,
                                };
                                if sink.send(msg).await.is_err() {
                                    break;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
                Ok(())
            },
        )
        .unwrap();

    module
}

// ── Upstream proxy ────────────────────────────────────────────────────────────

async fn proxy_call(upstream: &str, method: &str, params: Value) -> Result<Value, String> {
    debug!(method, %params, "→ upstream");

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(upstream)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            error!(method, err = %e, "HTTP send failed");
            e.to_string()
        })?;

    let status = resp.status();
    let json: Value = resp.json().await.map_err(|e| {
        error!(method, %status, err = %e, "Failed to parse upstream response");
        e.to_string()
    })?;

    if let Some(err) = json.get("error") {
        warn!(method, upstream_error = %err, "Upstream returned RPC error");
        return Err(err.to_string());
    }

    match json.get("result").cloned() {
        Some(result) => {
            debug!(method, %result, "← upstream ok");
            Ok(result)
        }
        None => {
            error!(method, response = %json, "Upstream response missing 'result' field");
            Err("missing result field".to_string())
        }
    }
}

fn rpc_err(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32000, msg, None::<()>)
}

// ── newHeads watcher ──────────────────────────────────────────────────────────

/// Connects to the upstream node via WebSocket and subscribes to `newHeads`.
/// Broadcasts every received header to the `tx` channel.
/// Retries with back-off on disconnection.
async fn watch_new_heads(rpc_url: &str, tx: broadcast::Sender<Value>) {
    use jsonrpsee::ws_client::WsClientBuilder;
    use jsonrpsee::core::client::SubscriptionClientT;

    let mut backoff = std::time::Duration::from_secs(2);

    loop {
        info!("Connecting to upstream WS: {}", rpc_url);
        let client = match WsClientBuilder::default().build(rpc_url).await {
            Ok(c) => c,
            Err(e) => {
                error!("WS connect failed: {e}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                continue;
            }
        };
        backoff = std::time::Duration::from_secs(2);

        let mut sub: jsonrpsee::core::client::Subscription<Value> = match client
            .subscribe("eth_subscribe", jsonrpsee::rpc_params!["newHeads"], "eth_unsubscribe")
            .await
        {
            Ok(s) => s,
            Err(e) => {
                error!("eth_subscribe failed: {e}");
                tokio::time::sleep(backoff).await;
                continue;
            }
        };

        info!("Subscribed to newHeads");
        loop {
            match sub.next().await {
                Some(Ok(header)) => {
                    let block_num = header
                        .get("number")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_owned();
                    debug!(block = %block_num, "new head received, broadcasting");
                    let receivers = tx.receiver_count();
                    if let Err(e) = tx.send(header) {
                        warn!("broadcast send failed (no receivers?): {e}");
                    } else {
                        debug!(block = %block_num, subscribers = receivers, "broadcast sent");
                    }
                }
                Some(Err(e)) => {
                    error!("Subscription error: {e}");
                    break;
                }
                None => {
                    info!("Subscription stream ended, reconnecting…");
                    break;
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
