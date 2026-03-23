use serde::Deserialize;

/// Sends and edits Telegram messages via the Bot API.
#[derive(Clone)]
pub struct TelegramNotifier {
    client: reqwest::Client,
    bot_token: String,
    chat_id: String,
    thread_id: Option<String>,
}

#[derive(Deserialize)]
struct SendResponse {
    ok: bool,
    result: Option<MessageResult>,
}

#[derive(Deserialize)]
struct MessageResult {
    message_id: i64,
}

impl TelegramNotifier {
    pub fn new(bot_token: &str, chat_id: &str, thread_id: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            bot_token: bot_token.to_string(),
            chat_id: chat_id.to_string(),
            thread_id,
        }
    }

    /// Send a status message and return the `message_id` on success.
    pub async fn send_status(&self, text: &str) -> Option<i64> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);

        let mut payload = serde_json::json!({
            "chat_id": &self.chat_id,
            "text": text,
        });
        if let Some(ref tid) = self.thread_id {
            payload["message_thread_id"] = serde_json::Value::String(tid.clone());
        }

        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .ok()?;

        let body: SendResponse = resp.json().await.ok()?;
        if body.ok {
            body.result.map(|r| r.message_id)
        } else {
            None
        }
    }

    /// Edit an existing message. Errors are silently ignored.
    pub async fn edit_status(&self, message_id: i64, text: &str) {
        let url = format!(
            "https://api.telegram.org/bot{}/editMessageText",
            self.bot_token
        );

        let _ = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": &self.chat_id,
                "message_id": message_id,
                "text": text,
                            }))
            .send()
            .await;
    }
}
