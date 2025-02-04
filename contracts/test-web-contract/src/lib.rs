use chrono::{DateTime, Utc};
use ed25519_dalek::Verifier;
use locutus_stdlib::{
    blake2::{Blake2b512, Digest},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Serialize, Deserialize)]
struct MessageFeed {
    messages: Vec<Message>,
}

#[derive(Serialize, Deserialize)]
struct Message {
    author: String,
    date: DateTime<Utc>,
    title: String,
    content: String,
    #[serde(default = "Message::modded")]
    mod_msg: bool,
    signature: Option<ed25519_dalek::Signature>,
}

impl Message {
    fn hash(&self) -> [u8; 64] {
        let mut hasher = Blake2b512::new();
        hasher.update(self.author.as_bytes());
        hasher.update(self.title.as_bytes());
        hasher.update(self.content.as_bytes());
        let hash_val = hasher.finalize();
        let mut key = [0; 64];
        key.copy_from_slice(&hash_val[..]);
        key
    }

    fn modded() -> bool {
        false
    }
}

impl<'a> TryFrom<State<'a>> for MessageFeed {
    type Error = ContractError;

    fn try_from(value: State<'a>) -> Result<Self, Self::Error> {
        serde_json::from_slice(value.as_ref()).map_err(|_| ContractError::InvalidState)
    }
}

#[derive(Serialize, Deserialize)]
struct FeedSummary {
    summaries: Vec<MessageSummary>,
}

impl<'a> From<&'a mut MessageFeed> for FeedSummary {
    fn from(feed: &'a mut MessageFeed) -> Self {
        feed.messages.sort_by_key(|m| m.date);
        let mut summaries = Vec::with_capacity(feed.messages.len());
        for msg in &feed.messages {
            summaries.push(MessageSummary(msg.hash()));
        }
        FeedSummary { summaries }
    }
}

#[serde_as]
#[derive(Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct MessageSummary(#[serde_as(as = "[_; 64]")] [u8; 64]);

impl<'a> TryFrom<StateSummary<'a>> for MessageSummary {
    type Error = ContractError;
    fn try_from(value: StateSummary<'a>) -> Result<Self, Self::Error> {
        serde_json::from_slice(&value).map_err(|_| ContractError::InvalidState)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Verification {
    public_key: ed25519_dalek::PublicKey,
}

impl Verification {
    fn verify(&self, msg: &Message) -> bool {
        if let Some(sig) = msg.signature {
            self.public_key
                .verify(&serde_json::to_vec(msg).unwrap(), &sig)
                .is_ok()
        } else {
            false
        }
    }
}

impl<'a> TryFrom<Parameters<'a>> for Verification {
    type Error = ContractError;
    fn try_from(value: Parameters<'a>) -> Result<Self, Self::Error> {
        serde_json::from_slice(value.as_ref()).map_err(|_| ContractError::InvalidState)
    }
}

#[contract]
impl ContractInterface for MessageFeed {
    fn validate_state(_parameters: Parameters<'static>, state: State<'static>) -> bool {
        MessageFeed::try_from(state).is_ok()
    }

    fn validate_delta(_parameters: Parameters<'static>, delta: StateDelta<'static>) -> bool {
        serde_json::from_slice::<Vec<Message>>(&delta).is_ok()
    }

    fn update_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        delta: StateDelta<'static>,
    ) -> Result<UpdateModification, ContractError> {
        let mut feed = MessageFeed::try_from(state)?;
        let verifier = Verification::try_from(parameters).ok();
        feed.messages.sort_by_cached_key(|m| m.hash());
        let mut incoming = serde_json::from_slice::<Vec<Message>>(&delta)
            .map_err(|_| ContractError::InvalidDelta)?;
        incoming.sort_by_cached_key(|m| m.hash());
        for m in incoming {
            if feed
                .messages
                .binary_search_by_key(&m.hash(), |o| o.hash())
                .is_err()
            {
                if m.mod_msg {
                    if let Some(verifier) = &verifier {
                        if !verifier.verify(&m) {
                            continue;
                        }
                        feed.messages.push(m);
                    }
                } else {
                    feed.messages.push(m);
                }
            }
        }
        Ok(UpdateModification::ValidUpdate(State::from(
            serde_json::to_vec(&feed).map_err(|err| ContractError::Other(err.into()))?,
        )))
    }

    fn summarize_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
    ) -> StateSummary<'static> {
        let mut feed = MessageFeed::try_from(state).unwrap();
        let only_messages = FeedSummary::from(&mut feed);
        StateSummary::from(serde_json::to_vec(&only_messages).expect("serialization failed"))
    }

    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> StateDelta<'static> {
        let feed = MessageFeed::try_from(state).unwrap();
        let mut summary = match serde_json::from_slice::<FeedSummary>(&summary) {
            Ok(summary) => summary,
            Err(_) => {
                // empty summary
                FeedSummary { summaries: vec![] }
            }
        };
        summary.summaries.sort();
        let mut final_messages = vec![];
        for msg in feed.messages {
            let mut hasher = Blake2b512::new();
            hasher.update(msg.author.as_bytes());
            hasher.update(msg.title.as_bytes());
            hasher.update(msg.content.as_bytes());
            let hash_val = hasher.finalize();
            if summary
                .summaries
                .binary_search_by(|m| m.0.as_ref().cmp(&hash_val[..]))
                .is_err()
            {
                final_messages.push(msg);
            }
        }
        StateDelta::from(serde_json::to_vec(&final_messages).unwrap())
    }

    // TODO: check if this is useful in anyway or makes sense at all
    fn update_state_from_summary(
        _parameters: Parameters<'static>,
        _state: State<'static>,
        _summary: StateSummary<'static>,
    ) -> Result<UpdateModification, ContractError> {
        unimplemented!()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn conversions() -> Result<(), Box<dyn std::error::Error>> {
        let json = r#"{
            "messages": [
                {
                    "author": "IDG",
                    "date": "2022-05-10T00:00:00Z",
                    "title": "Lore ipsum",
                    "content": "..."
                }
            ]
        }"#;
        let _feed = MessageFeed::try_from(State::from(json.as_bytes()))?;
        Ok(())
    }

    #[test]
    fn validate_state() {
        let json = r#"{
            "messages": [
                {
                    "author": "IDG",
                    "date": "2022-05-10T00:00:00Z",
                    "title": "Lore ipsum",
                    "content": "..."
                }
            ]
        }"#;
        let valid =
            MessageFeed::validate_state([].as_ref().into(), State::from(json.as_bytes().to_vec()));
        assert!(valid);
    }

    #[test]
    fn validate_delta() {
        let json = r#"[
            {
                "author": "IDG",
                "date": "2022-05-10T00:00:00Z",
                "title": "Lore ipsum",
                "content": "..."
            }
        ]"#;
        let valid = MessageFeed::validate_delta(
            [].as_ref().into(),
            StateDelta::from(json.as_bytes().to_vec()),
        );
        assert!(valid);
    }

    #[test]
    fn update_state() {
        let state = r#"{"messages":[{"author":"IDG","content":"...","date":"2022-05-10T00:00:00Z","title":"Lore ipsum"}]}"#;
        let delta =
            r#"[{"author":"IDG","content":"...","date":"2022-06-15T00:00:00Z","title":"New msg"}]"#;
        let new_state = MessageFeed::update_state(
            [].as_ref().into(),
            state.as_bytes().to_vec().into(),
            delta.as_bytes().to_vec().into(),
        )
        .unwrap()
        .unwrap_valid();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(new_state.as_ref()).unwrap(),
            serde_json::json!({
                "messages": [
                    {
                        "author": "IDG",
                        "date": "2022-05-10T00:00:00Z",
                        "title": "Lore ipsum",
                        "content": "..."
                    },
                    {
                        "author": "IDG",
                        "date": "2022-06-15T00:00:00Z",
                        "title": "New msg",
                        "content": "..."
                    }
                ]
            })
        );
    }

    #[test]
    fn summarize_state() {
        let json = r#"{
            "messages": [
                {
                    "author": "IDG",
                    "date": "2022-05-10T00:00:00Z",
                    "title": "Lore ipsum",
                    "content": "..."
                }
            ]
        }"#;
        let summary =
            MessageFeed::summarize_state([].as_ref().into(), State::from(json.as_bytes().to_vec()));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(summary.as_ref()).unwrap(),
            serde_json::json!([{
                "author": "IDG",
                "date": "2022-05-10T00:00:00Z",
                "title": "Lore ipsum",
                "content": "..."
            }])
        );
    }

    #[test]
    fn get_state_delta() {
        let json = r#"{
            "messages": [
                {
                    "author": "IDG",
                    "date": "2022-05-11T00:00:00Z",
                    "title": "Lore ipsum",
                    "content": "..."
                },
                {
                    "author": "IDG",
                    "date": "2022-04-10T00:00:00Z",
                    "title": "Lore ipsum",
                    "content": "..."
                }    
            ]
        }"#;
        let summary = serde_json::json!([{
                "author": "IDG",
                "date": "2022-04-10T00:00:00Z",
                "title": "Lore ipsum",
                "content": "..."
        }]);
        let delta = MessageFeed::get_state_delta(
            [].as_ref().into(),
            State::from(json.as_bytes().to_vec()),
            serde_json::to_vec(&summary).unwrap().into(),
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(delta.as_ref()).unwrap(),
            serde_json::json!([{
                "author": "IDG",
                "date": "2022-05-11T00:00:00Z",
                "title": "Lore ipsum",
                "content": "..."
            }])
        );
    }

    #[test]
    fn update_state_from_summary() {
        let state = r#"{
            "messages": [
                {
                    "author": "IDG",
                    "date": "2022-05-11T00:00:00Z",
                    "title": "Lore ipsum",
                    "content": "..."
                }    
            ]
        }"#;
        let summary = serde_json::json!([{
                "author": "IDG",
                "date": "2022-04-10T00:00:00Z",
                "title": "Lore ipsum",
                "content": "..."
        }]);
        let delta = MessageFeed::update_state_from_summary(
            [].as_ref().into(),
            State::from(state.as_bytes().to_vec()),
            serde_json::to_vec(&summary).unwrap().into(),
        )
        .unwrap()
        .unwrap_valid();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(delta.as_ref()).unwrap(),
            serde_json::json!({
                "messages": [
                    {
                        "author": "IDG",
                        "date": "2022-05-11T00:00:00Z",
                        "title": "Lore ipsum",
                        "content": "..."
                    },
                    {
                        "author": "IDG",
                        "date": "2022-04-10T00:00:00Z",
                        "title": "Lore ipsum",
                        "content": "..."
                    }
                ]
            })
        );
    }
}
