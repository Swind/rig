//! Discord ingress and outbound operations for the messaging router.
//!
//! The event handler spawns each turn. Admission uses the original address
//! before creating a thread or downloading bounded attachments.

use rig::{
    messaging::{
        Attachment, AttachmentSource, ChannelRef, ChatAdapter, ChatError, ChatRouter, Inbound,
        MessageRef, Sender, format::shorten_thread_name,
    },
    wasm_compat::{WasmBoxedFuture, WasmCompatSend, WasmCompatSync},
};
use serenity::all::{
    AutoArchiveDuration, Channel, ChannelId, Context, CreateAllowedMentions, CreateMessage,
    CreateThread, EditMessage, EventHandler, GuildChannel, Http, Message, MessageId, ReactionType,
    UserId,
};
use std::{num::NonZeroU64, sync::Arc};

const ATTACHMENT_LIMIT: usize = 10 * 1024 * 1024;
fn platform(
    error: impl std::error::Error + WasmCompatSend + WasmCompatSync + 'static,
) -> ChatError {
    ChatError::Platform(Box::new(error))
}
fn channel_id(ch: &ChannelRef) -> Result<ChannelId, std::num::ParseIntError> {
    Ok(ChannelId::new(ch.channel_id.parse::<NonZeroU64>()?.get()))
}
fn message_id(m: &MessageRef) -> Result<MessageId, std::num::ParseIntError> {
    Ok(MessageId::new(m.message_id.parse::<NonZeroU64>()?.get()))
}
fn mentions() -> CreateAllowedMentions {
    CreateAllowedMentions::new()
        .everyone(false)
        .all_users(false)
        .all_roles(false)
        .replied_user(false)
}

struct DiscordAdapter {
    http: Arc<Http>,
}
impl ChatAdapter for DiscordAdapter {
    fn platform(&self) -> &'static str {
        "discord"
    }
    fn message_limit(&self) -> usize {
        2000
    }
    fn send<'a>(
        &'a self,
        ch: &'a ChannelRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async move {
            let msg = channel_id(ch)
                .map_err(platform)?
                .send_message(
                    &self.http,
                    CreateMessage::new()
                        .content(text)
                        .allowed_mentions(mentions()),
                )
                .await
                .map_err(platform)?;
            Ok(MessageRef {
                channel: ch.clone(),
                message_id: msg.id.to_string(),
            })
        })
    }
    fn edit<'a>(
        &'a self,
        m: &'a MessageRef,
        text: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            channel_id(&m.channel)
                .map_err(platform)?
                .edit_message(
                    &self.http,
                    message_id(m).map_err(platform)?,
                    EditMessage::new()
                        .content(text)
                        .allowed_mentions(mentions()),
                )
                .await
                .map_err(platform)?;
            Ok(())
        })
    }
    fn delete<'a>(&'a self, m: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.http
                .delete_message(
                    channel_id(&m.channel).map_err(platform)?,
                    message_id(m).map_err(platform)?,
                    None,
                )
                .await
                .map_err(platform)
        })
    }
    fn add_reaction<'a>(
        &'a self,
        m: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.http
                .create_reaction(
                    channel_id(&m.channel).map_err(platform)?,
                    message_id(m).map_err(platform)?,
                    &ReactionType::Unicode(emoji.into()),
                )
                .await
                .map_err(platform)
        })
    }
    fn remove_reaction<'a>(
        &'a self,
        m: &'a MessageRef,
        emoji: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async move {
            self.http
                .delete_reaction_me(
                    channel_id(&m.channel).map_err(platform)?,
                    message_id(m).map_err(platform)?,
                    &ReactionType::Unicode(emoji.into()),
                )
                .await
                .map_err(platform)
        })
    }
}

fn normalize(msg: &Message, channel: &Channel, bot_id: UserId) -> Inbound {
    let original = ChannelRef {
        platform: "discord".into(),
        scope_id: msg.guild_id.map(|id| id.to_string()),
        channel_id: msg.channel_id.to_string(),
        thread_id: None,
    };
    Inbound {
        message: MessageRef {
            channel: original.clone(),
            message_id: msg.id.to_string(),
        },
        reply_channel: original,
        sender: Sender {
            id: msg.author.id.to_string(),
            name: msg
                .author
                .global_name
                .as_ref()
                .unwrap_or(&msg.author.name)
                .clone(),
            is_bot: msg.author.bot,
        },
        text: msg
            .content
            .replace(&format!("<@{bot_id}>"), "")
            .replace(&format!("<@!{bot_id}>"), "")
            .trim()
            .into(),
        attachments: vec![],
        is_dm: matches!(channel, Channel::Private(_)),
        is_thread: matches!(channel,Channel::Guild(gc) if gc.thread_metadata.is_some()),
        mentions_bot: msg.mentions_user_id(bot_id),
    }
}
fn use_thread(input: &mut Inbound, thread: &GuildChannel) {
    input.reply_channel.channel_id = thread.id.to_string();
}
fn attachment_fits(size: usize, used: usize) -> bool {
    size <= ATTACHMENT_LIMIT.saturating_sub(used)
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    remaining: usize,
) -> Result<Vec<u8>, ChatError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(platform)?
        .error_for_status()
        .map_err(platform)?;
    if response
        .content_length()
        .is_some_and(|size| size > remaining as u64)
    {
        return Err(platform(std::io::Error::other(
            "attachment exceeds size limit",
        )));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(platform)? {
        if chunk.len() > remaining.saturating_sub(bytes.len()) {
            return Err(platform(std::io::Error::other(
                "attachment exceeds size limit",
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) struct Handler {
    router: Arc<ChatRouter>,
    bot_id: UserId,
    downloads: reqwest::Client,
}
impl Handler {
    pub(crate) fn new(router: Arc<ChatRouter>, bot_id: UserId) -> Result<Self, reqwest::Error> {
        Ok(Self {
            router,
            bot_id,
            downloads: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
        })
    }
    async fn process(&self, http: Arc<Http>, msg: Message) -> Result<(), ChatError> {
        if msg.author.id == self.bot_id {
            return Ok(());
        }
        let channel = msg.channel_id.to_channel(&http).await.map_err(platform)?;
        let mut input = normalize(&msg, &channel, self.bot_id);
        if !self.router.allows(&input, &self.bot_id.to_string()) {
            return Ok(());
        }
        if !input.is_dm && !input.is_thread {
            let title = shorten_thread_name(&input.text);
            let title = if title.is_empty() {
                "Conversation"
            } else {
                &title
            };
            let thread = msg
                .channel_id
                .create_thread_from_message(
                    &http,
                    msg.id,
                    CreateThread::new(title).auto_archive_duration(AutoArchiveDuration::OneDay),
                )
                .await
                .map_err(platform)?;
            use_thread(&mut input, &thread);
        }
        let mut used = 0;
        for attachment in &msg.attachments {
            if !attachment_fits(attachment.size as usize, used) {
                input.text.push_str(&format!(
                    "\n[Attachment skipped: {} exceeds the 10 MiB total limit]",
                    attachment.filename
                ));
                continue;
            }
            match download(&self.downloads, &attachment.url, ATTACHMENT_LIMIT - used).await {
                Ok(bytes) => {
                    used += bytes.len();
                    input.attachments.push(Attachment {
                        filename: attachment.filename.clone(),
                        mime: attachment
                            .content_type
                            .clone()
                            .unwrap_or_else(|| "application/octet-stream".into()),
                        size: Some(bytes.len() as u64),
                        source: AttachmentSource::Bytes(bytes.into()),
                    });
                }
                Err(_) => input.text.push_str(&format!(
                    "\n[Attachment unavailable: {}]",
                    attachment.filename
                )),
            }
        }
        self.router
            .handle(
                Arc::new(DiscordAdapter { http }),
                input,
                &self.bot_id.to_string(),
            )
            .await
    }
}
#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        let handler = Self {
            router: self.router.clone(),
            bot_id: self.bot_id,
            downloads: self.downloads.clone(),
        };
        tokio::spawn(async move {
            if let Err(error) = handler.process(ctx.http, msg).await {
                eprintln!("Discord turn failed: {error}");
            }
        });
    }
}

#[cfg(test)]
#[path = "discord/tests.rs"]
mod tests;
