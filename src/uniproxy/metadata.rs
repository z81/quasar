//! Request metadata for remote function/state servers and the state prompt,
//! as TS builds it: the speaker biometry of MatchedUser events and the server
//! type.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value, json};

use crate::proto::n_alice::TAliceRequestProto;

/// The speaker as classified by the station (TS handleMatchedUserEvent)
#[derive(Clone, Copy, Debug)]
pub struct Biometry {
    pub age: &'static str,
    pub gender: &'static str,
}

/// The classification a MatchedUser event carries, if any
pub fn biometry(matched_user: &TAliceRequestProto) -> Option<Biometry> {
    let classification = matched_user.request.as_ref()?.event.as_ref()?.biometry_classification.as_ref()?;
    if classification.simple.is_empty() {
        return None;
    }
    let class_of = |tag: &str| {
        classification
            .simple
            .iter()
            .find(|item| item.tag.as_deref() == Some(tag))
            .and_then(|item| item.class_name.as_deref())
    };
    Some(Biometry {
        age: match class_of("children") {
            Some("adult") => "adult",
            Some("child") => "child",
            _ => "unknown",
        },
        gender: match class_of("gender") {
            Some("female") => "female",
            Some("male") => "male",
            _ => "unknown",
        },
    })
}

/// Biometry of one voice input: MatchedUser may arrive while the utterance is
/// still recorded or already being transcribed, so the turn reads it only
/// once the text is known (as TS reads its voice metadata)
#[derive(Clone, Default)]
pub struct BiometrySlot(Arc<Mutex<Option<Biometry>>>);

impl BiometrySlot {
    pub fn set(&self, biometry: Biometry) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(biometry);
    }

    pub fn get(&self) -> Option<Biometry> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What TS passes along with a request as `metadata`
pub struct InputMetadata {
    pub biometry: Option<Biometry>,
}

impl InputMetadata {
    pub fn new(biometry: Option<Biometry>) -> Self {
        Self { biometry }
    }

    /// The JSON remote servers receive: `age`/`gender` and
    /// `serverType` (TS PROCESSOR_METADATA_SERVER_TYPE_KEY)
    pub fn to_json(&self) -> Value {
        let mut metadata = Map::new();
        if let Some(biometry) = self.biometry {
            metadata.insert("age".to_string(), json!(biometry.age));
            metadata.insert("gender".to_string(), json!(biometry.gender));
        }
        metadata.insert("serverType".to_string(), json!("quasar"));
        Value::Object(metadata)
    }
}
