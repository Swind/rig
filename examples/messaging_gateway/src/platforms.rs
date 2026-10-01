use rig_messaging_platforms::{
    Error, Http, Incoming, Platform, feishu, line, lineworks, telegram, wecom,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};
type Configured = (
    Arc<dyn Platform>,
    Option<(
        mpsc::Receiver<Incoming>,
        tokio::task::JoinHandle<Result<(), Error>>,
    )>,
);
type ConfigError = Box<dyn std::error::Error>;
fn env(name: &str) -> Result<String, ConfigError> {
    Ok(std::env::var(name)?)
}
fn optional(name: &str) -> Option<String> {
    std::env::var(name).ok()
}
fn list(name: &str) -> Vec<String> {
    optional(name)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
fn private_key(name: &str) -> Result<String, ConfigError> {
    Ok(std::fs::read_to_string(env(name)?)?)
}
pub(crate) async fn configured(
    name: &str,
    shutdown: watch::Receiver<bool>,
) -> Result<Configured, ConfigError> {
    let http = Http::new(Duration::from_secs(60), 25 * 1024 * 1024)?;
    let platform: Arc<dyn Platform> = match name {
        "telegram" => {
            let mode = optional("TELEGRAM_MODE").unwrap_or_else(|| "webhook".into());
            if !matches!(mode.as_str(), "webhook" | "polling") {
                return Err(std::io::Error::other("unknown TELEGRAM_MODE").into());
            }
            let bot = Arc::new(telegram::Telegram::new(
                telegram::TelegramConfig {
                    bot_token: env("TELEGRAM_BOT_TOKEN")?,
                    bot_id: env("TELEGRAM_BOT_ID")?,
                    bot_username: env("TELEGRAM_BOT_USERNAME")?,
                    webhook_secret: env("TELEGRAM_WEBHOOK_SECRET")?,
                    api_base: "https://api.telegram.org".into(),
                    media_limit: 20 * 1024 * 1024,
                    rich_messages: true,
                },
                http,
            )?);
            if mode == "polling" {
                let (tx, rx) = mpsc::channel(128);
                let transport = bot.clone();
                let worker = tokio::spawn(async move {
                    let mut offset = 0;
                    let mut shutdown = shutdown;
                    loop {
                        let result = tokio::select! {
                            _=shutdown.changed()=>return Ok(()),
                            _=tx.closed()=>return Ok(()),
                            result=transport.poll(offset,50)=>result,
                        };
                        match result {
                            Ok((next, events)) => {
                                for event in events {
                                    tokio::select! {
                                        _=shutdown.changed()=>return Ok(()),
                                        sent=tx.send(event)=>if sent.is_err(){return Ok(());},
                                    }
                                }
                                offset = next;
                            }
                            Err(Error::Platform { code, .. }) if code == "401" || code == "403" => {
                                return Err(Error::Authentication);
                            }
                            Err(Error::Status(status)) if matches!(status.as_u16(), 401 | 403) => {
                                return Err(Error::Authentication);
                            }
                            Err(_) => {
                                eprintln!("Telegram polling failed; retrying");
                                tokio::select! {
                                    _=shutdown.changed()=>return Ok(()),
                                    _=tokio::time::sleep(Duration::from_secs(5))=>{},
                                }
                            }
                        }
                    }
                });
                return Ok((bot, Some((rx, worker))));
            }
            bot
        }
        "line" => Arc::new(line::Line::new(
            line::LineConfig {
                channel_secret: env("LINE_CHANNEL_SECRET")?,
                channel_access_token: env("LINE_CHANNEL_ACCESS_TOKEN")?,
                bot_id: env("LINE_BOT_ID")?,
                api_base: "https://api.line.me".into(),
                data_base: "https://api-data.line.me".into(),
                media_limit: 20 * 1024 * 1024,
            },
            http,
        )?),
        "lineworks" => Arc::new(lineworks::LineWorks::new(lineworks::LineWorksConfig {
            bot_id: env("LINEWORKS_BOT_ID")?,
            bot_secret: env("LINEWORKS_BOT_SECRET")?,
            bot_name: env("LINEWORKS_BOT_NAME")?,
            client_id: env("LINEWORKS_CLIENT_ID")?,
            client_secret: env("LINEWORKS_CLIENT_SECRET")?,
            service_account: env("LINEWORKS_SERVICE_ACCOUNT")?,
            private_key: private_key("LINEWORKS_PRIVATE_KEY_FILE")?,
            api_base: "https://www.worksapis.com/v1.0".into(),
            token_url: "https://auth.worksmobile.com/oauth2/v2.0/token".into(),
            media_limit: 20 * 1024 * 1024,
            media_hosts: list("LINEWORKS_MEDIA_HOSTS"),
            rich_messages: true,
        })?),
        "wecom" => Arc::new(wecom::WeCom::new(
            wecom::Config {
                corp_id: env("WECOM_CORP_ID")?,
                agent_id: env("WECOM_AGENT_ID")?.parse()?,
                secret: env("WECOM_SECRET")?,
                callback_token: env("WECOM_CALLBACK_TOKEN")?,
                encoding_aes_key: env("WECOM_ENCODING_AES_KEY")?,
            },
            http,
        )?),
        "feishu" | "lark" => {
            let mode = optional("FEISHU_MODE").unwrap_or_else(|| "websocket".into());
            if !matches!(mode.as_str(), "webhook" | "websocket") {
                return Err(std::io::Error::other("unknown FEISHU_MODE").into());
            }
            let mut config = feishu::Config::new(env("FEISHU_APP_ID")?, env("FEISHU_APP_SECRET")?);
            config.domain = if name == "lark" {
                feishu::Domain::Lark
            } else {
                feishu::Domain::Feishu
            };
            if mode == "webhook" {
                config.verification_token = Some(env("FEISHU_VERIFICATION_TOKEN")?);
                config.encrypt_key = Some(env("FEISHU_ENCRYPT_KEY")?);
            }
            let bot = Arc::new(feishu::Feishu::connect(config).await?);
            if mode == "webhook" {
                bot
            } else {
                let (tx, rx) = mpsc::channel(128);
                let transport = bot.clone();
                let worker =
                    tokio::spawn(async move { transport.run_websocket(tx, shutdown).await });
                return Ok((bot, Some((rx, worker))));
            }
        }
        "teams" => return teams(http),
        "googlechat" => return googlechat(),
        _ => return Err(std::io::Error::other("unknown MESSAGING_PLATFORM").into()),
    };
    Ok((platform, None))
}
fn teams(http: Http) -> Result<Configured, ConfigError> {
    use rig_messaging_platforms::teams::{Teams, TeamsConfig};
    let tenant = env("TEAMS_TENANT_ID")?;
    let platform = Teams::new(
        TeamsConfig {
            app_id: env("TEAMS_APP_ID")?,
            app_secret: env("TEAMS_APP_SECRET")?,
            tenant_id: tenant.clone(),
            oauth_endpoint: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
            jwks_url: "https://login.botframework.com/v1/.well-known/keys".into(),
            allowed_tenants: list("TEAMS_ALLOWED_TENANTS"),
            service_hosts: vec![
                "smba.trafficmanager.net".into(),
                "smba.infra.teams.microsoft.com".into(),
            ],
            media_hosts: list("TEAMS_MEDIA_HOSTS"),
            media_limit: 25 * 1024 * 1024,
        },
        http,
    )?;
    Ok((Arc::new(platform), None))
}
fn googlechat() -> Result<Configured, ConfigError> {
    use rig_messaging_platforms::googlechat::{
        GoogleChat, GoogleChatAuth, GoogleChatConfig, GoogleChatVerification,
    };
    let auth = if let Some(token) = optional("GOOGLE_CHAT_ACCESS_TOKEN") {
        GoogleChatAuth::StaticToken(token)
    } else if let Some(target_service_account) = optional("GOOGLE_CHAT_IMPERSONATE_ACCOUNT") {
        GoogleChatAuth::Impersonation {
            target_service_account,
        }
    } else {
        GoogleChatAuth::ServiceAccount {
            email: env("GOOGLE_CHAT_SERVICE_ACCOUNT")?,
            private_key: private_key("GOOGLE_CHAT_PRIVATE_KEY_FILE")?,
            token_url: "https://oauth2.googleapis.com/token".into(),
        }
    };
    let verification = match optional("GOOGLE_CHAT_VERIFICATION")
        .as_deref()
        .unwrap_or("endpoint")
    {
        "endpoint" => GoogleChatVerification::Endpoint,
        "project" => GoogleChatVerification::ProjectNumber,
        "addon" => GoogleChatVerification::WorkspaceAddon {
            service_account: env("GOOGLE_CHAT_ADDON_SIGNER")?,
        },
        _ => return Err(std::io::Error::other("unknown GOOGLE_CHAT_VERIFICATION").into()),
    };
    Ok((
        Arc::new(GoogleChat::new(GoogleChatConfig {
            bot_id: env("GOOGLE_CHAT_BOT_ID")?,
            audience: env("GOOGLE_CHAT_AUDIENCE")?,
            verification,
            auth,
            api_base: "https://chat.googleapis.com/v1".into(),
            media_limit: 25 * 1024 * 1024,
        })?),
        None,
    ))
}
