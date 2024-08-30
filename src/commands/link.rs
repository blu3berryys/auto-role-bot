use reqwest::StatusCode;
use serde::Deserialize;

use super::prelude::*;

#[derive(Deserialize)]
pub struct UserLookupResponse {
    pub account_id: i32,
    pub name: String,
}

#[poise::command(slash_command, guild_only = true)]
pub async fn link(
    ctx: Context<'_>,
    #[description = "GD username"] username: String,
) -> Result<(), CommandError> {
    if !username.is_ascii() || username.len() > 16 {
        ctx.reply(":x: Invalid username was provided.").await?;
        return Ok(());
    }

    let state = ctx.data();
    let member = ctx.author_member().await.unwrap();

    let response = match state
        .http_client
        .get(format!(
            "{}/gsp/lookup?username={}",
            state.base_url, username
        ))
        .header("Authorization", &state.server_password)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            ctx.reply(":x: Failed to make a request to the server!")
                .await?;
            bail!("User lookup failed: {e:?}");
        }
    };

    let status = response.status();
    if !status.is_success() {
        if status == StatusCode::NOT_FOUND {
            ctx.reply(":x: Failed to find the user by the given name. Make sure you are currently online on Globed and try again.").await?;
            return Ok(());
        }

        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "<no message>".to_owned());

        ctx.reply(":x: Server returned an unexpected error.")
            .await?;

        bail!(
            "User lookup failed: code {}, message: {}",
            status.as_u16(),
            message
        );
    }

    let json = response.text().await.unwrap_or_default();
    let response: UserLookupResponse = match serde_json::from_str(&json) {
        Ok(x) => x,
        Err(err) => {
            ctx.reply(":x: Server returned unparsable data.").await?;
            bail!("User lookup failed: failed to parse response: {err:?}\nResponse was: {json}");
        }
    };

    let user_id = ctx.author().id.get() as i64; // this is a relatively safe truncation (u64 -> i64, snowflake has to be below 2^63)

    match sqlx::query!(
        "INSERT INTO linked_users (id, gd_account_id) VALUES (?, ?)",
        user_id,
        response.account_id
    )
    .execute(&state.database)
    .await
    {
        Ok(_) => match state.sync_roles(&member).await {
            Ok(()) => {
                ctx.reply(format!(
                    "Linked <@{}> to GD account {} ({})!",
                    user_id, response.name, response.account_id
                ))
                .await?;
            }

            Err(err) => {
                warn!("Failed to sync roles: {err}");

                ctx.reply(format!(
                    "Linked <@{}> to GD account {} ({}) successfully, but role syncing failed. Try to execute the `/sync` command manually.",
                    user_id, response.name, response.account_id
                )).await?;
            }
        },

        Err(sqlx::Error::Database(err)) => {
            if err.message().contains("UNIQUE constraint failed") {
                // check if the someone else's discord is alreday linked to this gd account
                let linked_disc = match state.get_linked_discord_account(response.account_id).await
                {
                    Ok(x) => x,
                    Err(e) => {
                        ctx.reply(":x: Unknown database error has occurred.")
                            .await?;
                        bail!("database error: {e}");
                    }
                };

                // if linked to someone else than us, tell the user
                if linked_disc
                    .as_ref()
                    .is_some_and(|id| *id != ctx.author().id)
                {
                    let linked_id = linked_disc.unwrap();

                    // try to fetch the member and display their username, else fall back to their user id
                    let mut ident = String::new();

                    // god i fucking hate async rust
                    {
                        if let Some(cached) = ctx.cache().user(linked_id) {
                            ident.push('@');
                            ident.push_str(&cached.name);
                        }
                    }

                    if ident.is_empty() {
                        if let Ok(user) = ctx.http().get_user(linked_id).await {
                            ident.push('@');
                            ident.push_str(&user.name);
                        } else {
                            ident = linked_id.to_string();
                        }
                    };

                    ctx.reply(format!(":x: This Geometry Dash account is already linked to another Discord account ({}). If this is not you, please contact the moderator team.", ident))
                    .await?;
                } else {
                    // otherwise most likely we are already linked
                    ctx.reply(
                        ":x: Already linked. Use the `/unlink` command to unlink your account.",
                    )
                    .await?;
                }
                return Ok(());
            } else {
                ctx.reply(":x: Unknown database error has occurred.")
                    .await?;
                bail!("database error: {err}");
            }
        }

        Err(err) => {
            ctx.reply(":x: Unknown database error has occurred.")
                .await?;

            bail!("database connection error: {err}");
        }
    };

    Ok(())
}
