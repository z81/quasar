//! Read-only metadata from the same Home Assistant configured for MCP.
use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub fn websocket_url(address: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(address).ok()?;
    let (base, suffix) = url.path().split_once("/api/mcp")?;
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return None;
    }
    let path = format!("{base}/api/websocket");
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return None,
    };
    url.set_scheme(scheme).ok()?;
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

pub struct Registry {
    pub areas: Vec<Value>,
    pub devices: Vec<Value>,
    pub entities: Vec<Value>,
    pub states: Vec<Value>,
    pub exposed: Value,
}

pub async fn fetch(address: &str, token: &str) -> Result<Registry> {
    let url = websocket_url(address).context("not a Home Assistant endpoint")?;
    let (mut socket, _) = connect_async(url).await?;
    ensure!(
        receive(&mut socket).await?["type"] == "auth_required",
        "authentication protocol"
    );
    socket
        .send(Message::Text(
            json!({"type":"auth", "access_token": token})
                .to_string()
                .into(),
        ))
        .await?;
    ensure!(
        receive(&mut socket).await?["type"] == "auth_ok",
        "authentication failed"
    );
    // Require the exposure list. Reading all states must not expand Assist's scope.
    let exposed = request(&mut socket, 1, "homeassistant/expose_entity/list").await?;
    ensure!(
        exposed["exposed_entities"].is_object(),
        "invalid exposure list"
    );
    let areas = array(request(&mut socket, 2, "config/area_registry/list").await?)?;
    let devices = array(request(&mut socket, 3, "config/device_registry/list").await?)?;
    let entities = array(request(&mut socket, 4, "config/entity_registry/list").await?)?;
    let states = array(request(&mut socket, 5, "get_states").await?)?;
    let _ = socket.close(None).await;
    Ok(Registry {
        areas,
        devices,
        entities,
        states,
        exposed: exposed["exposed_entities"].clone(),
    })
}

fn array(value: Value) -> Result<Vec<Value>> {
    value
        .as_array()
        .cloned()
        .context("invalid registry response")
}

async fn request(socket: &mut Socket, id: u64, kind: &str) -> Result<Value> {
    socket
        .send(Message::Text(
            json!({"id": id, "type": kind}).to_string().into(),
        ))
        .await?;
    loop {
        let message = receive(socket).await?;
        if message["id"].as_u64() == Some(id) {
            ensure!(message["success"] == true, "registry request rejected");
            return Ok(message["result"].clone());
        }
    }
}

async fn receive(socket: &mut Socket) -> Result<Value> {
    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Ping(data) => socket.send(Message::Pong(data)).await?,
            Message::Close(_) => bail!("registry connection closed"),
            _ => {}
        }
    }
    bail!("registry connection ended")
}
