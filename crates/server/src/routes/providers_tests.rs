//! Server mode: shared and personal providers (`ai-provider-settings` spec).

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use super::accounts_tests::{Reply, account, call, get, post, server, sign_in};
use super::members_tests::send;
use crate::users::UserRole;

/// Somewhere nothing listens, so testing a provider fails at once without leaving the machine.
const NOWHERE: &str = "http://127.0.0.1:9/v1";

fn provider(name: &str, key: &str, shared: Option<bool>) -> Value {
    let mut body = json!({
        "name": name,
        "kind": "openai-compatible",
        "baseUrl": NOWHERE,
        "apiKey": key,
    });
    if let Some(shared) = shared {
        body["shared"] = json!(shared);
    }
    body
}

fn names(reply: &Reply) -> Vec<(String, bool)> {
    reply
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["name"].as_str().unwrap().to_owned(),
                p["shared"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn admins_share_providers_and_personal_ones_stay_private() {
    let (app, state) = server().await;
    account(&state, "boss", UserRole::Admin, "the admin password").await;
    account(&state, "lan", UserRole::User, "lan's own password").await;
    account(&state, "minh", UserRole::User, "minh's own password").await;
    let boss = sign_in(&app, "boss", "the admin password").await;
    let lan = sign_in(&app, "lan", "lan's own password").await;
    let minh = sign_in(&app, "minh", "minh's own password").await;
    let providers = "/api/settings/providers";
    let mut seen: Vec<(&str, Reply)> = Vec::new();

    // The admin shares a provider; a user cannot.
    let shared = call(
        &app,
        post(
            providers,
            Some(&boss),
            provider("OpenCode Go", "sk-shared-SECRET", Some(true)),
        ),
    )
    .await;
    assert_eq!(shared.status, StatusCode::CREATED, "{}", shared.json);
    assert_eq!(shared.json["shared"], true);
    let shared_id = shared.json["id"].as_str().unwrap().to_owned();
    let refused = call(
        &app,
        post(
            providers,
            Some(&minh),
            provider("Mine for all", "sk-x", Some(true)),
        ),
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.json["error"]["code"], "admins_only");

    // Lan adds her own key; without `shared`, a provider is personal, even an admin's.
    let personal = call(
        &app,
        post(
            providers,
            Some(&lan),
            provider("My OpenAI", "sk-lan-SECRET", None),
        ),
    )
    .await;
    assert_eq!(personal.status, StatusCode::CREATED);
    assert_eq!(personal.json["shared"], false);
    let lan_id = personal.json["id"].as_str().unwrap().to_owned();
    let boss_own = call(
        &app,
        post(
            providers,
            Some(&boss),
            provider("Boss's own", "sk-boss-SECRET", None),
        ),
    )
    .await;
    assert_eq!(boss_own.json["shared"], false);
    seen.extend([
        ("boss", shared),
        ("lan", personal),
        ("boss", boss_own),
        ("minh", refused),
    ]);

    // Each list: the shared provider and one's own.
    for (who, cookie, expected) in [
        (
            "lan",
            &lan,
            vec![("OpenCode Go", true), ("My OpenAI", false)],
        ),
        ("minh", &minh, vec![("OpenCode Go", true)]),
        (
            "boss",
            &boss,
            vec![("OpenCode Go", true), ("Boss's own", false)],
        ),
    ] {
        let listed = call(&app, get(providers, Some(cookie))).await;
        let expected: Vec<(String, bool)> = expected
            .into_iter()
            .map(|(name, shared)| (name.to_owned(), shared))
            .collect();
        assert_eq!(names(&listed), expected, "{who}");
        seen.push((who, listed));
    }

    // Lan's provider does not exist for Minh or the admin.
    let lan_provider = format!("{providers}/{lan_id}");
    for (who, cookie) in [("minh", &minh), ("boss", &boss)] {
        for request in [
            get(&lan_provider, Some(cookie)),
            send(
                Method::PATCH,
                &lan_provider,
                cookie,
                Some(json!({ "name": "Taken" })),
            ),
            send(Method::POST, &format!("{lan_provider}/test"), cookie, None),
            send(Method::DELETE, &lan_provider, cookie, None),
        ] {
            let path = format!("{} {}", request.method(), request.uri());
            let reply = call(&app, request).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{who} {path}");
            seen.push((who, reply));
        }
    }

    // Minh uses the shared provider but cannot change it; the admin can.
    let shared_provider = format!("{providers}/{shared_id}");
    let shown = call(&app, get(&shared_provider, Some(&minh))).await;
    assert_eq!(shown.status, StatusCode::OK);
    let tested = call(
        &app,
        send(
            Method::POST,
            &format!("{shared_provider}/test"),
            &minh,
            None,
        ),
    )
    .await;
    assert_eq!(tested.status, StatusCode::OK);
    assert_eq!(tested.json["ok"], false, "nothing listens there");
    seen.extend([("minh", shown), ("minh", tested)]);
    for request in [
        send(
            Method::PATCH,
            &shared_provider,
            &minh,
            Some(json!({ "apiKey": "sk-hijack" })),
        ),
        send(Method::DELETE, &shared_provider, &minh, None),
    ] {
        let reply = call(&app, request).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
    }
    let renamed = call(
        &app,
        send(
            Method::PATCH,
            &shared_provider,
            &boss,
            Some(json!({ "name": "OpenCode Go (lab)" })),
        ),
    )
    .await;
    assert_eq!(renamed.status, StatusCode::OK);
    assert_eq!(renamed.json["name"], "OpenCode Go (lab)");
    assert_eq!(renamed.json["shared"], true);

    // Lan manages her own.
    let lan_renamed = call(
        &app,
        send(
            Method::PATCH,
            &lan_provider,
            &lan,
            Some(json!({ "name": "OpenAI (Lan)" })),
        ),
    )
    .await;
    assert_eq!(lan_renamed.status, StatusCode::OK);
    seen.extend([("boss", renamed), ("lan", lan_renamed)]);

    // The other settings the admin reads.
    for path in ["/api/settings/roles", "/api/settings/provider-kinds"] {
        seen.push(("boss", call(&app, get(path, Some(&boss))).await));
    }

    // No response carries a key, and none shows the admin Lan's provider.
    for (who, reply) in &seen {
        let text = reply.json.to_string();
        assert!(!text.contains("SECRET"), "a key reached {who}: {text}");
        assert!(!text.contains("apiKey"), "{who}: {text}");
        if *who == "boss" {
            assert!(
                !text.contains(&lan_id) && !text.contains("My OpenAI"),
                "the admin saw Lan's provider: {text}"
            );
        }
    }

    assert_eq!(
        call(&app, send(Method::DELETE, &lan_provider, &lan, None))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
}
