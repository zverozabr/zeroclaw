use serde::Deserialize;
use tokio::task::JoinHandle;

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

        let resp = self.client.post(&url).json(&payload).send().await.ok()?;

        let body: SendResponse = resp.json().await.ok()?;
        if body.ok {
            body.result.map(|r| r.message_id)
        } else {
            None
        }
    }

    /// Start sending "typing" action every 4s. Returns handle to abort later.
    pub fn start_typing(&self) -> JoinHandle<()> {
        let client = self.client.clone();
        let bot_token = self.bot_token.clone();
        let chat_id = self.chat_id.clone();
        let thread_id = self.thread_id.clone();

        tokio::spawn(async move {
            loop {
                let url = format!("https://api.telegram.org/bot{}/sendChatAction", bot_token);
                let mut payload = serde_json::json!({
                    "chat_id": chat_id,
                    "action": "typing",
                });
                if let Some(ref tid) = thread_id {
                    payload["message_thread_id"] = serde_json::Value::String(tid.clone());
                }
                let _ = client.post(&url).json(&payload).send().await;
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
        })
    }

    /// Edit an existing message. Logs errors instead of silently ignoring.
    pub async fn edit_status(&self, message_id: i64, text: &str) {
        let url = format!(
            "https://api.telegram.org/bot{}/editMessageText",
            self.bot_token
        );

        let mut payload = serde_json::json!({
            "chat_id": &self.chat_id,
            "message_id": message_id,
            "text": text,
        });
        if let Some(ref tid) = self.thread_id {
            payload["message_thread_id"] = serde_json::Value::String(tid.clone());
        }

        match self.client.post(&url).json(&payload).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        status = %status,
                        body = %body.chars().take(200).collect::<String>(),
                        chat_id = %self.chat_id,
                        message_id,
                        "Pi Telegram edit_status failed"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Pi Telegram edit_status request failed");
            }
        }
    }
}
