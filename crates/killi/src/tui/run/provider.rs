//! `/provider` writes and list calls, each inside a busy spell so the keys stay live.

use super::super::*;

/// Applies a /provider pick: writes the preset's provider and models with the key, or
/// repairs the provider the config has, then re-reads the config so /model lists them at
/// once and a session already on that provider uses it as written from its next request.
/// The write itself runs inside a turn, so keys stay live while OpenCode Go's model list is
/// fetched.
pub(in crate::tui) async fn provider_add(
    app: &mut App,
    session: &mut Setup,
    agent: &mut Agent,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    preset: &str,
    key: &str,
) -> Result<()> {
    let outcome = Rc::new(RefCell::new(None));
    let slot = outcome.clone();
    let (preset, key) = (preset.to_string(), key.to_string());
    command_turn(app, keys, rx, async move {
        *slot.borrow_mut() = Some(match preset.as_str() {
            "opencode-go" => crate::login::opencode_go_add(Some(&key))
                .await
                .map(|go| (go.added, go.notes)),
            "command-code" => crate::login::commandcode_add(&key)
                .await
                .map(|code| (code.added, code.notes)),
            "anthropic" => crate::config::add_anthropic(&key).map(|added| (added, Vec::new())),
            "gemini" => crate::config::add_gemini(&key).map(|added| (added, Vec::new())),
            "grok" => crate::config::add_grok(&key).map(|added| (added, Vec::new())),
            "kimi" => crate::config::add_kimi(&key).map(|added| (added, Vec::new())),
            "mimo" => crate::config::add_mimo(&key).map(|added| (added, Vec::new())),
            "minimax" => crate::config::add_minimax(&key).map(|added| (added, Vec::new())),
            "qwen" => crate::config::add_qwen(&key).map(|added| (added, Vec::new())),
            "stepfun" => crate::config::add_stepfun(&key).map(|added| (added, Vec::new())),
            "unisound" => crate::config::add_unisound(&key).map(|added| (added, Vec::new())),
            "zai" => crate::config::add_zai(&key)
                .await
                .map(|added| (added, Vec::new())),
            _ => crate::config::add_muse(&key).map(|added| (added, Vec::new())),
        });
        Ok((String::new(), Stop::Completed))
    })
    .await?;
    let Some(outcome) = outcome.take() else {
        return Ok(());
    };
    provider_written(app, session, agent, outcome);
    Ok(())
}

/// `/provider custom` between the key and the ticks: reads the endpoint's model list
/// inside a turn (the key stays live while it runs), then offers the models to tick. A
/// failed list call ends the flow with the reason; nothing is written.
pub(in crate::tui) async fn provider_custom(
    app: &mut App,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    flow: CustomFlow,
) -> Result<()> {
    let outcome = Rc::new(RefCell::new(None));
    let slot = outcome.clone();
    // The closure takes copies: the answers also build the flow the ticked models write.
    let (ask, kept) = (
        (
            flow.protocol.clone(),
            flow.endpoint.clone(),
            flow.key.clone(),
        ),
        flow,
    );
    command_turn(app, keys, rx, async move {
        let (protocol, endpoint, key) = ask;
        *slot.borrow_mut() = Some(crate::config::custom_served(&protocol, &endpoint, &key).await);
        Ok((String::new(), Stop::Completed))
    })
    .await?;
    let Some(ids) = outcome.take() else {
        return Ok(());
    };
    match ids {
        Err(e) => app.error_from("/provider", &e),
        Ok(ids) => {
            app.notice(&format!(
                "The endpoint lists {}.",
                crate::tools::count(ids.len(), "model")
            ));
            app.custom = Some(kept);
            let options: Vec<_> = ids
                .into_iter()
                .map(|id| (id, String::new(), PickerChoice::Stay))
                .collect();
            let mut picker = Picker::new("Models to add".into(), options, 0);
            picker.multi = true;
            app.picker = Some(picker);
        }
    }
    Ok(())
}

/// `/provider custom`'s write: the provider and the ticked models, inside a turn so the
/// key stays live; then the same reload and notices as a preset.
pub(in crate::tui) async fn provider_custom_add(
    app: &mut App,
    session: &mut Setup,
    agent: &mut Agent,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    flow: CustomFlow,
    models: Vec<String>,
) -> Result<()> {
    let outcome = Rc::new(RefCell::new(None));
    let slot = outcome.clone();
    let CustomFlow {
        name,
        protocol,
        endpoint,
        key,
    } = flow;
    command_turn(app, keys, rx, async move {
        *slot.borrow_mut() = Some(
            crate::config::add_custom(&name, &protocol, &endpoint, &key, &models)
                .map(|added| (added, Vec::new())),
        );
        Ok((String::new(), Stop::Completed))
    })
    .await?;
    let Some(outcome) = outcome.take() else {
        return Ok(());
    };
    provider_written(app, session, agent, outcome);
    Ok(())
}

/// What a /provider write leaves: the notices, the reload, and the model switch when the
/// session sat on the provider or on a model it no longer serves.
fn provider_written(
    app: &mut App,
    session: &mut Setup,
    agent: &mut Agent,
    outcome: anyhow::Result<(crate::config::Added, Vec<String>)>,
) {
    match outcome {
        Err(e) => app.error_from("/provider", &e),
        Ok((added, notes)) => {
            if added.aliases.is_empty() {
                app.notice("Its models are already in the config; /model picks one.");
            } else {
                app.notice(&format!(
                    "Added {} — /model picks one.",
                    added.aliases.join(", ")
                ));
                if let Some(default) = &added.starts_on {
                    app.notice(&format!(
                        "killi starts on `{default}`; /model picks another."
                    ));
                }
            }
            if let Some(note) = added.repair_note() {
                app.notice(&note);
            }
            if !added.dropped.is_empty() {
                app.notice(&format!(
                    "Removed models no longer served: {}.",
                    added.dropped.join(", ")
                ));
            }
            for note in &added.notes {
                app.notice(note);
            }
            for note in &notes {
                app.notice(note);
            }
            if let Err(e) = session.reload() {
                app.error_from("/reload", &e);
                return;
            }
            let alias = agent.model_alias().to_string();
            let on_it = session
                .config()
                .models
                .get(&alias)
                .is_some_and(|model| model.provider == added.provider);
            if on_it {
                match session.config().resolve(Some(&alias)).and_then(Client::new) {
                    Ok(client) => match agent.switch_model(client) {
                        Ok(()) => {
                            app.show_model(agent, session.config());
                            app.notice("This session takes it from its next request.");
                        }
                        Err(e) => app.error_report(&e),
                    },
                    Err(e) => app.error_report(&e),
                }
            } else if !session.config().models.contains_key(&alias) {
                // The list sync removed the model this session was on; move to one the
                // provider still serves rather than leaving the status on a stale alias.
                let first = session
                    .config()
                    .models
                    .iter()
                    .find(|(name, model)| {
                        model.provider == added.provider
                            && session.config().model_allowed(name)
                            && !session.config().missing_key(Some(name))
                    })
                    .map(|(name, _)| name.clone());
                if let Some(first) = first {
                    match session.config().resolve(Some(&first)).and_then(Client::new) {
                        Ok(client) => match agent.switch_model(client) {
                            Ok(()) => {
                                app.show_model(agent, session.config());
                                app.notice(&provider_switch_notice(&alias, &first));
                            }
                            Err(e) => app.error_report(&e),
                        },
                        Err(e) => app.error_report(&e),
                    }
                }
            }
        }
    }
}

/// The model switch after `/provider`: first configuration has no removed model to name.
pub(in crate::tui) fn provider_switch_notice(alias: &str, first: &str) -> String {
    if alias.is_empty() {
        format!("Now on `{first}`; /model picks another.")
    } else {
        format!("`{alias}` is no longer served; switched to `{first}`.")
    }
}
