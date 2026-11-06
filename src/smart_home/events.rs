//! Decode only known native IoT envelopes; never log raw callbacks or networks.
use super::*;
use crate::proto::google::protobuf::Any;
use crate::proto::n_alice::n_alice_api::{
    t_client_message, TCapabilityUpdateState, TEnvironmentState,
};
use crate::proto::n_alice::{
    t_alice_request_proto::t_request::EnvironmentStateType, t_event, t_typed_callback_request,
    TAliceRequestProto, TFinishDiscoveryCallback, TFinishSystemDiscoveryCallback,
    TTypedCallbackRequest,
};

mod json_callback;

impl Registry {
    pub fn observe(&self, duid: &str, connection_id: u64, event: &t_client_message::TEvent) {
        use t_client_message::t_event::Event;
        let mut stations = self.0.lock().unwrap();
        let station = stations.entry(duid.into()).or_default();
        if connection_id < station.connection_id {
            return;
        }
        if station.connection_id != connection_id {
            *station = Station {
                connection_id,
                ..Default::default()
            };
        }
        match event.event.as_ref() {
            Some(Event::CapabilityUpdateState(update)) => station.update(update),
            Some(Event::TextInput(request) | Event::VoiceInput(request)) => {
                station.request(request)
            }
            Some(Event::SyncPushResponse(response)) => {
                use crate::proto::n_alice::n_alice_api::t_sync_response::Response;
                if let Some(Response::GetDeviceStateResponse(response)) = response
                    .sync_response
                    .as_ref()
                    .and_then(|response| response.response.as_ref())
                {
                    if let Some(environment) = &response.environment_state {
                        station.endpoints(environment.endpoints.clone());
                    }
                }
            }
            _ => {}
        }
    }
}

impl Station {
    fn update(&mut self, event: &TCapabilityUpdateState) {
        if let Some(holder) = &event.any_capability {
            self.capability(
                &event.endpoint_id,
                if holder.capability_type == 0 {
                    event.capability_type
                } else {
                    holder.capability_type
                },
                &holder.raw_capability,
            );
        }
        if let Some(capability) = &event.capability {
            let kind = match capability.type_url.rsplit('/').next() {
                Some("NAlice.TIotDiscoveryCapability") => {
                    ECapabilityType::IotDiscoveryCapabilityType as i32
                }
                Some("NAlice.TOnOffCapability") => ECapabilityType::OnOffCapabilityType as i32,
                Some("NAlice.TLevelCapability") => ECapabilityType::LevelCapabilityType as i32,
                _ => event.capability_type,
            };
            self.capability(&event.endpoint_id, kind, &capability.value);
        }
    }

    fn capability(&mut self, endpoint: &str, kind: i32, raw: &[u8]) {
        if raw.is_empty() {
            return;
        }
        if kind == ECapabilityType::IotDiscoveryCapabilityType as i32 {
            let Ok(update) = TIotDiscoveryCapability::decode(raw) else {
                return;
            };
            if self.hub_endpoint != endpoint {
                self.capability = None;
                self.hub_endpoint = endpoint.into();
            }
            let capability = self.capability.get_or_insert_default();
            if update.meta.is_some() {
                capability.meta = update.meta;
            }
            if update.parameters.is_some() {
                capability.parameters = update.parameters;
            }
            if let Some(state) = update.state {
                if let Some(session) = self.session.as_mut() {
                    if state.is_discovery_in_progress && session.status == "queued" {
                        session.status = "searching";
                        session.message =
                            "Станция ищет устройства. Переведите устройство в режим сопряжения."
                                .into();
                    } else if !state.is_discovery_in_progress && session.status == "cancelling" {
                        session.status = "cancelled";
                        session.message = "Станция завершила поиск.".into();
                    }
                }
                capability.state = Some(state);
            }
        } else if let Some(device) = self.endpoints.get_mut(endpoint) {
            device.capability(kind, raw);
        }
    }

    fn endpoints(&mut self, endpoints: Vec<TEndpoint>) {
        // Environment snapshots can be partial. Keep existing endpoints until
        // disconnect; never infer removals from an omitted endpoint.
        for endpoint in endpoints.into_iter().take(256) {
            for holder in &endpoint.any_capabilities {
                if holder.capability_type == ECapabilityType::IotDiscoveryCapabilityType as i32 {
                    self.capability(&endpoint.id, holder.capability_type, &holder.raw_capability);
                }
            }
            for capability in &endpoint.capabilities {
                if capability
                    .type_url
                    .ends_with("/NAlice.TIotDiscoveryCapability")
                {
                    self.capability(
                        &endpoint.id,
                        ECapabilityType::IotDiscoveryCapabilityType as i32,
                        &capability.value,
                    );
                }
            }
            let previous = self.endpoints.get(&endpoint.id);
            if let Some(entry) = Endpoint::reported(endpoint, previous) {
                if self.endpoints.len() < 256 || self.endpoints.contains_key(&entry.endpoint.id) {
                    self.endpoints.insert(entry.endpoint.id.clone(), entry);
                }
            }
        }
    }

    fn request(&mut self, request: &TAliceRequestProto) {
        let Some(request) = &request.request else {
            return;
        };
        match &request.environment_state_type {
            Some(EnvironmentStateType::EnvironmentState(environment)) => {
                self.endpoints(environment.endpoints.clone())
            }
            Some(EnvironmentStateType::EnvironmentStateRaw(raw)) => {
                if let Ok(environment) = TEnvironmentState::decode(raw.as_slice()) {
                    self.endpoints(environment.endpoints);
                }
            }
            _ => {}
        }
        let Some(event) = &request.event else {
            return;
        };
        let name = event.name.as_deref().unwrap_or_default();
        if let Some(t_event::PayloadType::Payload(payload)) = &event.payload_type {
            if name == "@@mm_typed_callback" {
                self.callback_json(&crate::directives::struct_to_json(payload));
            }
            return;
        }
        let Some(t_event::PayloadType::PayloadRaw(raw)) = &event.payload_type else {
            return;
        };
        if name == "@@mm_typed_callback" {
            if let Ok(callback) = TTypedCallbackRequest::decode(raw.as_slice()) {
                match callback.callback_payload {
                    Some(t_typed_callback_request::CallbackPayload::TypedCallbackSerialized(
                        bytes,
                    )) => {
                        if let Ok(any) = Any::decode(bytes.as_slice()) {
                            self.typed(&any.type_url, &any.value);
                        }
                    }
                    Some(t_typed_callback_request::CallbackPayload::TypedCallbackStruct(value)) => {
                        if let Some(payload) = value.payload {
                            self.typed_json(
                                &value.typed_callback_name,
                                &crate::directives::struct_to_json(&payload),
                            );
                        }
                    }
                    _ => {}
                }
            }
            if let Ok(any) = Any::decode(raw.as_slice()) {
                self.typed(&any.type_url, &any.value);
            }
        } else if name == "@@mm_semantic_frame" {
            use crate::proto::n_alice::n_alice_api::{
                t_typed_semantic_frame::Type, TSemanticFrameRequestData,
            };
            if let Ok(frame) = TSemanticFrameRequestData::decode(raw.as_slice()) {
                if let Some(Type::EndpointEventsBatchSemanticFrame(frame)) =
                    frame.typed_semantic_frame.and_then(|f| f.r#type)
                {
                    if let Some(
                        crate::proto::n_alice::t_endpoint_events_batch_slot::Value::BatchValue(
                            batch,
                        ),
                    ) = frame.batch.and_then(|batch| batch.value)
                    {
                        for endpoint in batch.batch {
                            if let Some(status) = endpoint.endpoint_status {
                                if let Some(stored) = self.endpoints.get_mut(&endpoint.endpoint_id)
                                {
                                    stored.endpoint.status = Some(status);
                                }
                            }
                            for event in endpoint.capability_events {
                                if let Some(event) = event.event {
                                    self.capability_event(&endpoint.endpoint_id, &event);
                                }
                            }
                        }
                    }
                }
            }
        } else {
            self.typed(name, raw);
        }
    }

    fn capability_event(&mut self, endpoint_id: &str, event: &Any) {
        match event.type_url.rsplit('/').next().unwrap_or_default() {
            "NAlice.TIotDiscoveryCapability.TUpdateStateEvent" => {
                if let Ok(event) = discovery::TUpdateStateEvent::decode(event.value.as_slice()) {
                    if let Some(capability) = event.capability {
                        self.capability(
                            endpoint_id,
                            ECapabilityType::IotDiscoveryCapabilityType as i32,
                            &capability.encode_to_vec(),
                        );
                    }
                }
            }
            "NAlice.TOnOffCapability.TUpdateStateEvent" => {
                if let Ok(event) =
                    crate::proto::n_alice::t_on_off_capability::TUpdateStateEvent::decode(
                        event.value.as_slice(),
                    )
                {
                    if let Some(capability) = event.capability {
                        self.capability(
                            endpoint_id,
                            ECapabilityType::OnOffCapabilityType as i32,
                            &capability.encode_to_vec(),
                        );
                    }
                }
            }
            "NAlice.TLevelCapability.TUpdateStateEvent" => {
                if let Ok(event) =
                    crate::proto::n_alice::t_level_capability::TUpdateStateEvent::decode(
                        event.value.as_slice(),
                    )
                {
                    if let Some(capability) = event.capability {
                        self.capability(
                            endpoint_id,
                            ECapabilityType::LevelCapabilityType as i32,
                            &capability.encode_to_vec(),
                        );
                    }
                }
            }
            _ => self.typed(&event.type_url, &event.value),
        }
    }

    fn typed(&mut self, name: &str, raw: &[u8]) {
        match name.rsplit('/').next().unwrap_or(name) {
            "NAlice.TFinishDiscoveryCallback" | "iot_finish_discovery_callback" => {
                if let Ok(callback) = TFinishDiscoveryCallback::decode(raw) {
                    self.found(callback);
                }
            }
            "NAlice.TFinishSystemDiscoveryCallback" | "iot_finish_system_discovery_callback" => {
                if let Ok(callback) = TFinishSystemDiscoveryCallback::decode(raw) {
                    self.endpoints(callback.discovered_endpoints);
                }
            }
            "NAlice.TIotDiscoveryCapability.TIotDiscoveryCompleteEvent" => {
                if let Ok(event) = discovery::TIotDiscoveryCompleteEvent::decode(raw) {
                    if let Some(session) =
                        self.session.as_mut().filter(|s| s.id == event.session_id)
                    {
                        let error = match event.result {
                            Some(discovery::t_iot_discovery_complete_event::Result::Matter(
                                result,
                            )) => result.error,
                            _ => None,
                        };
                        session.callback_id = None;
                        if let Some(error) = error {
                            session.status = "failed";
                            session.message = format!(
                                "Matter: станция сообщила ошибку {}, этап {}.",
                                error.code, error.matter_discovery_stage
                            );
                        } else {
                            session.status = "complete";
                            session.message = format!(
                                "Станция завершила поиск. Найдено устройств: {}.",
                                event.found_devices_count
                            );
                        }
                    }
                }
            }
            "NAlice.TIotDiscoveryCapability.TIotDiscoveryProgressEvent" => {
                if let Ok(event) = discovery::TIotDiscoveryProgressEvent::decode(raw) {
                    if let Some(session) =
                        self.session.as_mut().filter(|s| s.id == event.session_id)
                    {
                        session.message = "Станция выполняет подключение Matter-устройства.".into();
                        if session.status == "queued" {
                            session.status = "searching";
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn found(&mut self, callback: TFinishDiscoveryCallback) {
        let Some(session) = self.session.as_mut().filter(|s| {
            s.id == callback.session_id
                && s.expires_at >= now()
                && matches!(s.status, "queued" | "searching" | "awaiting_confirmation")
        }) else {
            return;
        };
        let expected = if session.protocol == "zigbee" { 1 } else { 2 };
        session.candidates = callback
            .discovered_endpoints
            .into_iter()
            .filter_map(Endpoint::new)
            .map(|e| e.view())
            .filter(|e| e["protocol_code"] == expected)
            .take(16)
            .collect();
        session.callback_id = Some(callback.callback_id);
        session.system_info = callback.system_info;
        session.status = "awaiting_confirmation";
        session.message = if session.candidates.is_empty() {
            "Станция завершила поиск без доступных устройств."
        } else {
            "Станция нашла устройство. Подтвердите подключение."
        }
        .into();
    }
}
