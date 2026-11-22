//! `open-prism-api user …`: accounts from the command line, for whoever runs the server
//! (`user-accounts` spec, design D10). It opens the data dir's database directly, which SQLite
//! allows while the server runs. Passwords are never arguments: the commands print one-time
//! links, so no password ends up in the shell history.

use std::path::Path;

use anyhow::Context;
use sqlx::Row;

use crate::cli::UserCommand;
use crate::db::now_ms;
use crate::links;
use crate::mode::Mode;
use crate::users::{self, UserRole};

/// Runs a `user` command and returns what to print.
pub async fn run(data_dir: &Path, mode: &Mode, command: &UserCommand) -> anyhow::Result<String> {
    let Some(server) = mode.server() else {
        anyhow::bail!(
            "the user commands manage the accounts of server mode: add --mode server and \
             --public-url, as for `serve` (or set OPENPRISM_MODE and OPENPRISM_PUBLIC_URL)"
        );
    };
    let db = crate::db::open(&data_dir.join("open-prism.db")).await?;
    let now = now_ms();
    match command {
        UserCommand::List => {
            let rows = sqlx::query(
                "SELECT username, display_name, role, disabled, password_hash IS NOT NULL AS has_password,
                        last_login_at
                 FROM users ORDER BY username COLLATE NOCASE",
            )
            .fetch_all(&db)
            .await?;
            if rows.is_empty() {
                return Ok("No accounts yet. Create the first admin with \
                           `open-prism-api user create-admin <name>`, or open the link the \
                           server prints when it starts."
                    .into());
            }
            let mut out = format!("{:<24} {:<6} {:<12} {}\n", "USER", "ROLE", "STATUS", "NAME");
            for row in rows {
                let username: String = row.try_get("username")?;
                let role: String = row.try_get("role")?;
                let disabled: bool = row.try_get("disabled")?;
                let has_password: bool = row.try_get("has_password")?;
                let name: String = row.try_get("display_name")?;
                let status = if disabled {
                    "locked"
                } else if has_password {
                    "active"
                } else {
                    "no password"
                };
                out.push_str(&format!("{username:<24} {role:<6} {status:<12} {name}\n"));
            }
            Ok(out.trim_end().to_owned())
        }
        UserCommand::ResetPassword { username } => {
            let record = users::find_by_username(&db, username)
                .await?
                .with_context(|| format!("no account named {username}"))?;
            if record.disabled {
                anyhow::bail!("{username} is locked: an admin must unlock the account first");
            }
            let token = links::set_password(&db, &record.user.id, now).await?;
            Ok(format!(
                "Open this link to set a new password for {} (it works once, for 7 days):\n{}",
                record.user.username,
                server.public_url.link(&format!("/setup/{token}"))
            ))
        }
        UserCommand::CreateAdmin {
            username,
            display_name,
        } => {
            let username = username.trim();
            users::check_username(username).map_err(anyhow::Error::msg)?;
            let display_name = display_name.as_deref().unwrap_or(username).trim();
            if display_name.is_empty() {
                anyhow::bail!("the display name is empty");
            }
            let mut tx = db.begin().await?;
            let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ?")
                .bind(username)
                .fetch_one(&mut *tx)
                .await?;
            if taken > 0 {
                anyhow::bail!(
                    "the user name {username} is taken; to give that account a new password, run \
                     `open-prism-api user reset-password {username}`"
                );
            }
            let first: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
                .fetch_one(&mut *tx)
                .await?;
            let admin =
                users::create(&mut *tx, username, display_name, UserRole::Admin, now).await?;
            if first == 0 {
                // The first account takes over what single-user mode left (design D9).
                links::adopt_unowned(&mut tx, &admin.id, now).await?;
            }
            tx.commit().await?;
            let token = links::set_password(&db, &admin.id, now).await?;
            Ok(format!(
                "Created the admin {username}. Open this link to set the password (it works once, \
                 for 7 days):\n{}",
                server.public_url.link(&format!("/setup/{token}"))
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::ModeArg;

    fn server() -> Mode {
        Mode::resolve(ModeArg::Server, Some("https://prism.example.org"), false).unwrap()
    }

    #[tokio::test]
    async fn needs_server_mode() {
        let dir = tempfile::tempdir().unwrap();
        let err = run(dir.path(), &Mode::Single, &UserCommand::List)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--mode server"), "{err}");
    }

    #[tokio::test]
    async fn create_admin_then_list_and_reset_while_the_server_runs() {
        let dir = tempfile::tempdir().unwrap();
        // The server has the database open the whole time.
        let state = crate::state::AppState::open_in(
            server(),
            dir.path(),
            crate::auth::AccessToken::new("a".repeat(64)).unwrap(),
            Vec::new(),
        )
        .await
        .unwrap();
        let now = now_ms();
        sqlx::query("INSERT INTO projects (id, name, main_file, created_at, updated_at) VALUES ('p1', 'Paper', 'main.tex', ?, ?)")
            .bind(now)
            .bind(now)
            .execute(&state.db)
            .await
            .unwrap();

        let empty = run(dir.path(), &server(), &UserCommand::List)
            .await
            .unwrap();
        assert!(empty.contains("No accounts yet"), "{empty}");

        let created = run(
            dir.path(),
            &server(),
            &UserCommand::CreateAdmin {
                username: "lan".into(),
                display_name: Some("Lan Tran".into()),
            },
        )
        .await
        .unwrap();
        let link = created.lines().last().unwrap();
        assert!(
            link.starts_with("https://prism.example.org/setup/"),
            "{created}"
        );
        // The running server sees the account, and the first admin took over the project.
        assert!(
            users::find_by_username(&state.db, "lan")
                .await
                .unwrap()
                .is_some()
        );
        let owner: String = sqlx::query_scalar("SELECT u.username FROM project_members m JOIN users u ON u.id = m.user_id WHERE m.role = 'owner'")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(owner, "lan");
        let token = link.rsplit('/').next().unwrap();
        let mut conn = state.db.acquire().await.unwrap();
        assert!(
            links::find(&mut conn, token, now_ms())
                .await
                .unwrap()
                .is_some()
        );

        let taken = run(
            dir.path(),
            &server(),
            &UserCommand::CreateAdmin {
                username: "LAN".into(),
                display_name: None,
            },
        )
        .await
        .unwrap_err();
        assert!(taken.to_string().contains("taken"), "{taken}");

        let listed = run(dir.path(), &server(), &UserCommand::List)
            .await
            .unwrap();
        assert!(
            listed.contains("lan") && listed.contains("admin") && listed.contains("no password"),
            "{listed}"
        );

        let reset = run(
            dir.path(),
            &server(),
            &UserCommand::ResetPassword {
                username: "lan".into(),
            },
        )
        .await
        .unwrap();
        let new_token = reset.lines().last().unwrap().rsplit('/').next().unwrap();
        // The new link replaces the one from create-admin.
        assert!(
            links::find(&mut conn, token, now_ms())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            links::find(&mut conn, new_token, now_ms())
                .await
                .unwrap()
                .is_some()
        );

        let missing = run(
            dir.path(),
            &server(),
            &UserCommand::ResetPassword {
                username: "nobody".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(missing.to_string().contains("no account named nobody"));
    }
}
