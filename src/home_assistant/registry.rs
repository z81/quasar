//! Authenticated HA transport. Commands use one exact, freshly validated entity.
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
    pub services: Value,
    pub temperature_unit: Option<String>,
}

pub async fn fetch(address: &str, token: &str) -> Result<Registry> {
    let mut socket = connect(address, token).await?;
    let registry = snapshot(&mut socket).await?;
    let _ = socket.close(None).await;
    Ok(registry)
}

async fn connect(address: &str, token: &str) -> Result<Socket> {
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
    Ok(socket)
}

async fn snapshot(socket: &mut Socket) -> Result<Registry> {
    // Require the exposure list. Reading all states must not expand Assist's scope.
    let exposed = request(socket, 1, "homeassistant/expose_entity/list").await?;
    ensure!(
        exposed["exposed_entities"].is_object(),
        "invalid exposure list"
    );
    let areas = array(request(socket, 2, "config/area_registry/list").await?)?;
    let devices = array(request(socket, 3, "config/device_registry/list").await?)?;
    let entities = array(request(socket, 4, "config/entity_registry/list").await?)?;
    let states = array(request(socket, 5, "get_states").await?)?;
    // Missing optional capability metadata must not take the state dashboard down.
    let services = request(socket, 6, "get_services")
        .await
        .unwrap_or(Value::Null);
    let config = request(socket, 7, "get_config")
        .await
        .unwrap_or(Value::Null);
    Ok(Registry {
        areas,
        devices,
        entities,
        states,
        exposed: exposed["exposed_entities"].clone(),
        services,
        temperature_unit: config["unit_system"]["temperature"]
            .as_str()
            .map(str::to_owned),
    })
}

pub async fn execute(
    address: &str,
    token: &str,
    action: &super::controls::Action,
) -> Result<(), super::controls::ActionError> {
    use super::controls::{self, ActionError};
    let mut socket = connect(address, token)
        .await
        .map_err(|_| ActionError::Unavailable)?;
    let registry = snapshot(&mut socket)
        .await
        .map_err(|_| ActionError::Unavailable)?;
    let command = controls::prepare(&registry, action)?;
    // No retries: a lost acknowledgement must never send the physical action twice.
    let result = request_message(&mut socket, 8, command)
        .await
        .map_err(|_| ActionError::Unconfirmed);
    let _ = socket.close(None).await;
    result.map(|_| ())
}

fn array(value: Value) -> Result<Vec<Value>> {
    value
        .as_array()
        .cloned()
        .context("invalid registry response")
}

async fn request(socket: &mut Socket, id: u64, kind: &str) -> Result<Value> {
    request_message(socket, id, json!({"type": kind})).await
}

async fn request_message(socket: &mut Socket, id: u64, mut request: Value) -> Result<Value> {
    request["id"] = json!(id);
    socket
        .send(Message::Text(request.to_string().into()))
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
