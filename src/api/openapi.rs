//! OpenAPI document of the management API: the one the TS server's Elysia
//! openapi plugin serves (`/openapi/json`, quasarConfig schema generated from
//! `station_config`), and the Scalar page that renders it (`/openapi`).
//! Bodies are documented as JSON and form-encoded, the ones accepted here.

use std::sync::LazyLock;

use axum::http::header;
use axum::response::{Html, IntoResponse};
use serde_json::{Value, json};

use crate::station_config;

static DOCUMENT: LazyLock<String> = LazyLock::new(|| build().to_string());

const PAGE: &str = r#"<!doctype html>
<html>
  <head>
    <title>Quasar API</title>
    <meta name="description" content="Development documentation" />
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
  </head>
  <body>
    <noscript><a href="openapi/json">OpenAPI document (JSON)</a></noscript>
    <script id="api-reference" data-configuration='{"url":"openapi/json"}'></script>
    <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@latest/dist/browser/standalone.min.js" crossorigin></script>
  </body>
</html>
"#;

pub async fn page() -> Html<&'static str> {
    Html(PAGE)
}

pub async fn document() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], DOCUMENT.as_str())
}

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
const DUID_OR_ALL: &str = "DUID or 'all' for all devices";

fn build() -> Value {
    let mut quasar_config = station_config::json_schema();
    quasar_config["description"] = json!("Quasar's maind config");
    let device = json!({
        "type": "object",
        "properties": {
            "duid": { "type": "string", "description": "DUID" },
            "name": { "type": "string", "description": "Name" },
            "quasarConfig": quasar_config,
        },
        "required": ["duid", "name", "quasarConfig"],
        "additionalProperties": false,
    });
    let mut device_response = device.clone();
    device_response["$schema"] = json!(DIALECT);
    let update_body = json!({
        "$schema": DIALECT,
        "type": "object",
        "properties": {
            "name": { "description": "Name", "type": "string" },
            "quasarConfig": quasar_config,
        },
        "additionalProperties": false,
    });
    let empty = json!({ "type": "object", "properties": {} });

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Quasar API",
            "description": "Development documentation",
            "version": "1.4.6",
        },
        "paths": {
            "/devices": {
                "get": operation(
                    "Get list of devices",
                    "getDevices",
                    None,
                    None,
                    json!({ "$schema": DIALECT, "type": "array", "items": device }),
                ),
            },
            "/devices/{duid}/push": {
                "post": operation(
                    "Push event to LLM to be processed",
                    "postDevicesByDuidPush",
                    Some(DUID_OR_ALL),
                    Some(request_body(event_text_schema("Event text to be pushed to LLM"), None)),
                    empty.clone(),
                ),
            },
            "/devices/{duid}/push-raw": {
                "post": operation(
                    "Push event text to be TTSed",
                    "postDevicesByDuidPush-raw",
                    Some(DUID_OR_ALL),
                    Some(request_body(event_text_schema("Event text to be pushed to TTS"), None)),
                    empty.clone(),
                ),
            },
            "/devices/{duid}/push-directive": {
                "post": operation(
                    "Push directive to the device",
                    "postDevicesByDuidPush-directive",
                    Some(DUID_OR_ALL),
                    Some(request_body(
                        json!({ "description": "Raw Quasar directive" }),
                        Some("Raw Quasar directive"),
                    )),
                    empty,
                ),
            },
            "/devices/{duid}": {
                "patch": operation(
                    "Update device config",
                    "patchDevicesByDuid",
                    Some("DUID"),
                    Some(request_body(update_body, None)),
                    device_response,
                ),
            },
        },
        "components": { "schemas": {} },
    })
}

fn operation(
    summary: &str,
    operation_id: &str,
    duid_description: Option<&str>,
    request_body: Option<Value>,
    response: Value,
) -> Value {
    let mut operation = json!({
        "summary": summary,
        "tags": ["device"],
        "responses": {
            "200": {
                "description": "Response for status 200",
                "content": { "application/json": { "schema": response } },
            },
        },
        "operationId": operation_id,
    });
    if let Some(description) = duid_description {
        operation["parameters"] = json!([{
            "name": "duid",
            "in": "path",
            "required": true,
            "schema": { "description": description, "type": "string" },
        }]);
    }
    if let Some(request_body) = request_body {
        operation["requestBody"] = request_body;
    }
    operation
}

fn request_body(schema: Value, description: Option<&str>) -> Value {
    let mut body = json!({
        "required": true,
        "content": {
            "application/json": { "schema": schema },
            "application/x-www-form-urlencoded": { "schema": schema },
        },
    });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    body
}

fn event_text_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "required": ["eventText"],
        "properties": { "eventText": { "description": description, "type": "string" } },
    })
}
