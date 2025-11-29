use std::collections::BTreeMap;

use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use mistralrs::{TextMessageRole, TextMessages};
use provider_core::{ChatMessage, ChatRole, Model, ModelConfig, ModelId, Provider, ProviderId};

pub const MISTRAL_RS_ID: &str = "mistralrs";

pub struct MistralRs {
    models: BTreeMap<ModelId, MistralRsModel>,
}

pub struct MistralRsModel {
    mistral_rs_model: mistralrs::Model,
    model: Model,
}

#[async_trait::async_trait]
impl Provider for MistralRs {
    fn get_provider_id(&self) -> ProviderId {
        MISTRAL_RS_ID.to_string()
    }

    async fn list_models(&self) -> Result<Vec<Model>> {
        Ok(self.models.values().map(|x| x.model.clone()).collect())
    }

    async fn register_model(&mut self, model: ModelConfig) -> Result<()> {
        let mistral_rs_model = mistralrs::TextModelBuilder::new(model.id.clone())
            .build()
            .await
            .map_err(|e| eyre!(e))?;
        let model = Model {
            id: model.id.clone(),
            name: model.id,
            max_context: model.max_context,
        };
        self.models.insert(
            model.id.clone(),
            MistralRsModel {
                mistral_rs_model,
                model,
            },
        );
        Ok(())
    }

    async fn generate_reply(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let mut tmessages = TextMessages::new();
        for m in messages {
            let role = match m.role {
                ChatRole::System => TextMessageRole::System,
                ChatRole::User => TextMessageRole::User,
                ChatRole::Assistant => TextMessageRole::Assistant,
                ChatRole::Tool => TextMessageRole::Tool,
            };
            tmessages = tmessages.add_message(role, m.content);
        }

        let response = self
            .models
            .get(model_id)
            .unwrap()
            .mistral_rs_model
            .send_chat_request(tmessages)
            .await
            .map_err(|e| eyre!(e))?;

        let out = ChatMessage {
            role: ChatRole::Assistant,
            content: response
                .choices
                .first()
                .unwrap()
                .message
                .content
                .clone()
                .unwrap(),
        };
        Ok(out)
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        messages: Vec<ChatMessage>,
    ) -> Result<BoxStream<'static, Result<String>>> {
        todo!()
    }
}
