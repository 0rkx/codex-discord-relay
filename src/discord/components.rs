use reqwest::Url;
use serenity::{
    all::{ButtonStyle, InputTextStyle},
    builder::{
        CreateActionRow, CreateButton, CreateInputText, CreateSelectMenu, CreateSelectMenuKind,
        CreateSelectMenuOption,
    },
};

use crate::discord::actions::{ActionField, FieldKind};
use crate::security::MAX_PASSWORD_LENGTH;

pub const NEW_TASK: &str = "relay:new_task";
pub const REFRESH_TASKS: &str = "relay:refresh_tasks";
pub const GOD_START: &str = "relay:god_start";
pub const GOD_STOP: &str = "relay:god_stop";
pub const TASK_CONTINUE: &str = "relay:task_continue";
pub const TASK_INTERRUPT: &str = "relay:task_interrupt";
pub const TASK_FORK: &str = "relay:task_fork";
pub const TASK_ARCHIVE: &str = "relay:task_archive";
pub const EMAIL_START: &str = "relay:email_start";
pub const ACTION_BROWSER: &str = "relay:action_browser";
pub const ACTION_CATEGORY: &str = "relay:action_category";
pub const ACTION_METHOD: &str = "relay:action_method";
pub const ACTION_PAGE: &str = "relay:action_page";
pub const ACTION_CONTINUE: &str = "relay:action_continue";
pub const ACTION_EXECUTE: &str = "relay:action_execute";
pub const ACTION_CANCEL: &str = "relay:action_cancel";
pub const APPROVE_ONCE: &str = "relay:approve_once";
pub const APPROVE_SESSION: &str = "relay:approve_session";
pub const DENY: &str = "relay:deny";
pub const CANCEL_REQUEST: &str = "relay:cancel_request";
pub const APPROVE_OFFERED: &str = "relay:approval_offer";
pub const ANSWER_REQUEST: &str = "relay:answer_request";
pub const NEW_TASK_MODAL: &str = "relay:new_task_modal";
pub const GOD_MODAL: &str = "relay:god_modal";
pub const CONTINUE_MODAL: &str = "relay:continue_modal";
pub const SERVER_ANSWER_MODAL: &str = "relay:server_answer_modal";
pub const EMAIL_MODAL: &str = "relay:email_modal";
pub const ACTION_FORM_MODAL: &str = "relay:action_form";
pub const TASK_BROWSER_PAGE: &str = "relay:tasks_page";
pub const MODEL_SELECT: &str = "relay:model_select";
pub const TERMINAL_KILL: &str = "relay:terminal_kill";
pub const TERMINAL_CLEAN: &str = "relay:terminal_clean";
pub const MODE_SELECT: &str = "relay:mode_select";
pub const TYPED_INPUT_SELECT: &str = "relay:typed_input";
pub const TYPED_INPUT_MODAL: &str = "relay:typed_input_modal";
pub const PLUGIN_ACTIONS: &str = "relay:plugin_actions";
pub const PLUGIN_SELECT: &str = "relay:plugin_select";
pub const PLUGIN_INSTALL: &str = "relay:plugin_install";
pub const PLUGIN_UNINSTALL: &str = "relay:plugin_uninstall";
pub const PLUGIN_BACK: &str = "relay:plugin_back";
pub const PLUGIN_PAGE: &str = "relay:plugin_page";
pub const REALTIME_START: &str = "relay:realtime_start";
pub const REALTIME_TEXT: &str = "relay:realtime_text";
pub const REALTIME_VOICES: &str = "relay:realtime_voices";
pub const REALTIME_STOP: &str = "relay:realtime_stop";

/// The argument carried after a `prefix:` custom id, if `id` uses that prefix.
#[must_use]
pub fn custom_id_arg<'a>(id: &'a str, prefix: &str) -> Option<&'a str> {
    id.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix(':'))
}

#[must_use]
pub fn control_buttons() -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(NEW_TASK)
            .label("New task")
            .emoji('➕')
            .style(ButtonStyle::Primary),
        CreateButton::new(REFRESH_TASKS)
            .label("Browse tasks")
            .emoji('🗂')
            .style(ButtonStyle::Secondary),
        CreateButton::new(ACTION_BROWSER)
            .label("Actions")
            .emoji('⚡')
            .style(ButtonStyle::Secondary),
    ])]
}

#[must_use]
pub fn task_buttons(done: bool) -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::Buttons(vec![
            CreateButton::new(TASK_CONTINUE)
                .label("Continue")
                .emoji('▶')
                .style(ButtonStyle::Primary),
            CreateButton::new(TASK_INTERRUPT)
                .label("Stop turn")
                .emoji('⏹')
                .style(ButtonStyle::Danger)
                .disabled(done),
            CreateButton::new(GOD_START)
                .label("GOD mode")
                .emoji('🔴')
                .style(ButtonStyle::Danger),
        ]),
        CreateActionRow::Buttons(vec![
            CreateButton::new(TASK_FORK)
                .label("Fork")
                .emoji('🔀')
                .style(ButtonStyle::Secondary),
            CreateButton::new(TASK_ARCHIVE)
                .label("Archive")
                .emoji('📦')
                .style(ButtonStyle::Secondary),
            CreateButton::new(EMAIL_START)
                .label("Email")
                .emoji('✉')
                .style(ButtonStyle::Secondary),
            CreateButton::new(ACTION_BROWSER)
                .label("Actions")
                .emoji('⚡')
                .style(ButtonStyle::Secondary),
            CreateButton::new(REALTIME_START)
                .label("Realtime")
                .emoji('🎙')
                .style(ButtonStyle::Secondary),
        ]),
    ]
}

#[must_use]
pub fn realtime_buttons(active: bool) -> Vec<CreateActionRow> {
    let mut buttons = vec![
        CreateButton::new(if active {
            REALTIME_TEXT
        } else {
            REALTIME_START
        })
        .label(if active {
            "Send text"
        } else {
            "Start realtime"
        })
        .style(ButtonStyle::Primary),
        CreateButton::new(REALTIME_VOICES)
            .label("Voices")
            .style(ButtonStyle::Secondary),
    ];
    if active {
        buttons.push(
            CreateButton::new(REALTIME_STOP)
                .label("Stop realtime")
                .style(ButtonStyle::Danger),
        );
    }
    vec![CreateActionRow::Buttons(buttons)]
}

#[must_use]
pub fn action_category_select(
    categories: impl IntoIterator<Item = (String, usize)>,
) -> Vec<CreateActionRow> {
    let options = categories
        .into_iter()
        .take(25)
        .map(|(category, count)| {
            CreateSelectMenuOption::new(humanize(&category), category)
                .description(format!("{count} Codex actions"))
        })
        .collect();
    vec![CreateActionRow::SelectMenu(
        CreateSelectMenu::new(ACTION_CATEGORY, CreateSelectMenuKind::String { options })
            .placeholder("Choose an action family")
            .min_values(1)
            .max_values(1),
    )]
}

#[must_use]
pub fn typed_input_select(
    token: &str,
    noun: &str,
    choices: impl IntoIterator<Item = (usize, String, String)>,
) -> Vec<CreateActionRow> {
    let options = choices
        .into_iter()
        .take(25)
        .map(|(index, label, description)| {
            CreateSelectMenuOption::new(
                safe_single_line(&label, 100, "Unnamed choice"),
                index.to_string(),
            )
            .description(safe_single_line(
                &description,
                100,
                "Use this choice with Codex",
            ))
        })
        .collect::<Vec<_>>();
    if options.is_empty() {
        return Vec::new();
    }
    vec![CreateActionRow::SelectMenu(
        CreateSelectMenu::new(
            format!("{TYPED_INPUT_SELECT}:{token}"),
            CreateSelectMenuKind::String { options },
        )
        .placeholder(truncate(&format!("Use a {noun}"), 150))
        .min_values(1)
        .max_values(1),
    )]
}

#[must_use]
pub fn typed_input_prompt(noun: &str) -> Vec<CreateActionRow> {
    vec![CreateActionRow::InputText(
        CreateInputText::new(
            InputTextStyle::Paragraph,
            truncate(&format!("What should Codex do with this {noun}?"), 45),
            "prompt",
        )
        .placeholder("Give Codex the task or question")
        .required(true)
        .max_length(4_000),
    )]
}

#[must_use]
pub fn action_method_picker(
    category: &str,
    page: usize,
    methods: impl IntoIterator<Item = (String, String, String)>,
    has_previous: bool,
    has_next: bool,
) -> Vec<CreateActionRow> {
    let options = methods
        .into_iter()
        .take(25)
        .map(|(method, label, description)| {
            CreateSelectMenuOption::new(truncate(&label, 100), method)
                .description(truncate(&description, 100))
        })
        .collect();
    let mut rows = vec![CreateActionRow::SelectMenu(
        CreateSelectMenu::new(ACTION_METHOD, CreateSelectMenuKind::String { options })
            .placeholder(format!("Choose a {category} action"))
            .min_values(1)
            .max_values(1),
    )];
    if has_previous || has_next {
        rows.push(CreateActionRow::Buttons(vec![
            CreateButton::new(format!(
                "{ACTION_PAGE}:{category}:{}",
                page.saturating_sub(1)
            ))
            .label("Previous")
            .emoji('◀')
            .style(ButtonStyle::Secondary)
            .disabled(!has_previous),
            CreateButton::new(format!("{ACTION_PAGE}:{category}:{}", page + 1))
                .label("Next")
                .emoji('▶')
                .style(ButtonStyle::Secondary)
                .disabled(!has_next),
        ]));
    }
    rows
}

#[must_use]
pub fn action_form_inputs(fields: &[ActionField]) -> Vec<CreateActionRow> {
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let mut input = CreateInputText::new(
                if matches!(
                    field.kind,
                    FieldKind::Array | FieldKind::Object | FieldKind::Json
                ) {
                    InputTextStyle::Paragraph
                } else {
                    InputTextStyle::Short
                },
                truncate(&field.label, 45),
                format!("f:{index}"),
            )
            .placeholder(field.placeholder())
            .required(field.required && field.default.is_none())
            .max_length(4000);
            if let Some(default) = field.default.as_ref() {
                input = input.value(default_text(default));
            }
            CreateActionRow::InputText(input)
        })
        .collect()
}

#[must_use]
pub fn action_continue_button(token: &str, page: usize) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{ACTION_CONTINUE}:{token}:{page}"))
            .label("Continue setup")
            .emoji('▶')
            .style(ButtonStyle::Primary),
        CreateButton::new(format!("{ACTION_CANCEL}:{token}"))
            .label("Cancel")
            .style(ButtonStyle::Secondary),
    ])]
}

#[must_use]
pub fn action_confirm_buttons(token: &str, requires_god: bool) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{ACTION_EXECUTE}:{token}"))
            .label(if requires_god {
                "Execute with GOD"
            } else {
                "Run action"
            })
            .emoji(if requires_god { '🔴' } else { '▶' })
            .style(if requires_god {
                ButtonStyle::Danger
            } else {
                ButtonStyle::Success
            }),
        CreateButton::new(format!("{ACTION_CANCEL}:{token}"))
            .label("Cancel")
            .style(ButtonStyle::Secondary),
    ])]
}

fn default_text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn humanize(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_else(|| "Other".to_owned())
}

fn truncate(value: &str, max: usize) -> String {
    let mut output = value.chars().take(max).collect::<String>();
    if value.chars().count() > max && max > 1 {
        output.pop();
        output.push('…');
    }
    output
}

fn safe_single_line(value: &str, max: usize, fallback: &str) -> String {
    let cleaned = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(
        if cleaned.is_empty() {
            fallback
        } else {
            &cleaned
        },
        max,
    )
}

#[must_use]
pub fn approval_buttons(request_id: &str) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{APPROVE_ONCE}:{request_id}"))
            .label("Approve once")
            .style(ButtonStyle::Success),
        CreateButton::new(format!("{APPROVE_SESSION}:{request_id}"))
            .label("Approve session")
            .style(ButtonStyle::Primary),
        CreateButton::new(format!("{DENY}:{request_id}"))
            .label("Deny")
            .style(ButtonStyle::Danger),
        CreateButton::new(format!("{CANCEL_REQUEST}:{request_id}"))
            .label("Cancel and stop")
            .style(ButtonStyle::Secondary),
    ])]
}

#[must_use]
pub fn offered_approval_buttons(
    request_id: &str,
    choices: impl IntoIterator<Item = (usize, &'static str)>,
) -> Vec<CreateActionRow> {
    let buttons = choices
        .into_iter()
        .take(25)
        .map(|(index, label)| {
            let style = if label.starts_with("Decline") {
                ButtonStyle::Danger
            } else if label.starts_with("Cancel") {
                ButtonStyle::Secondary
            } else if label.contains("session") {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Success
            };
            CreateButton::new(format!("{APPROVE_OFFERED}:{request_id}:{index}"))
                .label(label)
                .style(style)
        })
        .collect::<Vec<_>>();
    buttons
        .chunks(5)
        .take(5)
        .map(|row| CreateActionRow::Buttons(row.to_vec()))
        .collect()
}

#[must_use]
pub fn answer_buttons(request_id: &str) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{ANSWER_REQUEST}:{request_id}"))
            .label("Answer")
            .style(ButtonStyle::Primary),
        CreateButton::new(format!("{DENY}:{request_id}"))
            .label("Decline")
            .style(ButtonStyle::Danger),
    ])]
}

#[must_use]
pub fn plugin_manage_buttons() -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(PLUGIN_ACTIONS)
            .label("Manage plugins")
            .emoji('🧩')
            .style(ButtonStyle::Primary),
    ])]
}

#[must_use]
pub fn plugin_browser_controls(
    token: &str,
    options: impl IntoIterator<Item = (usize, String, String)>,
    page: usize,
    pages: usize,
) -> Vec<CreateActionRow> {
    let options = options
        .into_iter()
        .map(|(index, name, description)| {
            CreateSelectMenuOption::new(truncate(&name, 100), index.to_string())
                .description(truncate(&description, 100))
        })
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    if !options.is_empty() {
        rows.push(CreateActionRow::SelectMenu(CreateSelectMenu::new(
            format!("{PLUGIN_SELECT}:{token}"),
            CreateSelectMenuKind::String { options },
        )));
    }
    if pages > 1 {
        rows.push(CreateActionRow::Buttons(vec![
            CreateButton::new(format!(
                "{PLUGIN_PAGE}:{token}:{}",
                page.saturating_sub(1).max(1)
            ))
            .label("Previous")
            .emoji('◀')
            .style(ButtonStyle::Secondary)
            .disabled(page <= 1),
            CreateButton::new(format!("{PLUGIN_PAGE}:{token}:{}", (page + 1).min(pages)))
                .label("Next")
                .emoji('▶')
                .style(ButtonStyle::Secondary)
                .disabled(page >= pages),
        ]));
    }
    rows.extend(plugin_manage_buttons());
    rows
}

#[must_use]
pub fn plugin_detail_buttons(
    token: &str,
    index: usize,
    install_action: Option<bool>,
) -> Vec<CreateActionRow> {
    let mut buttons = Vec::new();
    if let Some(install) = install_action {
        buttons.push(if install {
            CreateButton::new(format!("{PLUGIN_INSTALL}:{token}:{index}"))
                .label("Install")
                .emoji('⬇')
                .style(ButtonStyle::Success)
        } else {
            CreateButton::new(format!("{PLUGIN_UNINSTALL}:{token}:{index}"))
                .label("Uninstall")
                .emoji('🗑')
                .style(ButtonStyle::Danger)
        });
    }
    buttons.extend([
        CreateButton::new(format!("{PLUGIN_BACK}:{token}:{index}"))
            .label("Back")
            .style(ButtonStyle::Secondary),
        CreateButton::new(PLUGIN_ACTIONS)
            .label("All plugin actions")
            .style(ButtonStyle::Secondary),
    ]);
    vec![CreateActionRow::Buttons(buttons)]
}

#[must_use]
pub fn plugin_auth_buttons(
    apps: impl IntoIterator<Item = (String, String)>,
) -> Vec<CreateActionRow> {
    link_button_rows("Authenticate", '🔐', apps)
}

/// Link buttons that open an app's chatgpt.com install page ("Can be
/// installed" in official TUI wording).
#[must_use]
pub fn connector_link_buttons(
    apps: impl IntoIterator<Item = (String, String)>,
) -> Vec<CreateActionRow> {
    link_button_rows("Install", '⬇', apps)
}

/// External setup/auth links are HTTPS-only; plain http is rejected.
fn link_button_rows(
    verb: &str,
    emoji: char,
    items: impl IntoIterator<Item = (String, String)>,
) -> Vec<CreateActionRow> {
    let buttons = items
        .into_iter()
        .filter_map(|(name, url)| safe_https_url(&url).map(|url| (name, url)))
        .take(20)
        .map(|(name, url)| {
            CreateButton::new_link(url)
                .label(truncate(&format!("{verb} {name}"), 80))
                .emoji(emoji)
        })
        .collect::<Vec<_>>();
    buttons
        .chunks(5)
        .map(|chunk| CreateActionRow::Buttons(chunk.to_vec()))
        .collect()
}

/// Accept only bounded HTTPS links without embedded credentials. Returned
/// value is normalized before it reaches a Discord link button.
#[must_use]
pub(crate) fn safe_https_url(raw: &str) -> Option<String> {
    if raw.len() > 2_048 {
        return None;
    }
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    Some(parsed.into())
}

#[must_use]
pub fn elicitation_confirmation_buttons(request_id: &str) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{APPROVE_ONCE}:{request_id}"))
            .label("Accept")
            .style(ButtonStyle::Success),
        CreateButton::new(format!("{DENY}:{request_id}"))
            .label("Decline")
            .style(ButtonStyle::Danger),
    ])]
}

#[must_use]
pub fn elicitation_url_buttons(request_id: &str, url: Option<&str>) -> Vec<CreateActionRow> {
    let mut buttons = Vec::new();
    if let Some(url) = url.and_then(safe_https_url) {
        buttons.push(
            CreateButton::new_link(url)
                .label("Open secure page")
                .emoji('🔗'),
        );
    }
    buttons.extend([
        CreateButton::new(format!("{APPROVE_ONCE}:{request_id}"))
            .label("I completed it")
            .style(ButtonStyle::Success),
        CreateButton::new(format!("{CANCEL_REQUEST}:{request_id}"))
            .label("Cancel")
            .style(ButtonStyle::Secondary),
        CreateButton::new(format!("{DENY}:{request_id}"))
            .label("Decline")
            .style(ButtonStyle::Danger),
    ]);
    vec![CreateActionRow::Buttons(buttons)]
}

pub const OPEN_TASK: &str = "relay:open_task";
pub const OPEN_TASK_ARCHIVED: &str = "relay:open_task_archived";

/// Task picker. `archived` selections route through an unarchive step before
/// the task is materialized, so reopened tasks can start turns again.
pub fn task_select(
    tasks: impl IntoIterator<Item = (String, String, String)>,
    archived: bool,
) -> CreateActionRow {
    let options = tasks
        .into_iter()
        .take(25)
        .map(|(id, title, status)| CreateSelectMenuOption::new(title, id).description(status))
        .collect();
    let custom_id = if archived {
        OPEN_TASK_ARCHIVED
    } else {
        OPEN_TASK
    };
    CreateActionRow::SelectMenu(
        CreateSelectMenu::new(custom_id, CreateSelectMenuKind::String { options })
            .placeholder("Choose a Codex task")
            .min_values(1)
            .max_values(1),
    )
}

pub fn model_select(
    models: impl IntoIterator<Item = (String, String, String)>,
    current: Option<&str>,
) -> CreateActionRow {
    let mut options = vec![
        CreateSelectMenuOption::new("Task default (clear override)", "__default__")
            .description("Use the configured Codex default model")
            .default_selection(current.is_none()),
    ];
    options.extend(models.into_iter().take(24).map(|(id, label, description)| {
        CreateSelectMenuOption::new(label, id.clone())
            .description(description)
            .default_selection(current == Some(id.as_str()))
    }));
    CreateActionRow::SelectMenu(
        CreateSelectMenu::new(MODEL_SELECT, CreateSelectMenuKind::String { options })
            .placeholder("Choose the model for new turns")
            .min_values(1)
            .max_values(1),
    )
}

/// Select-and-buttons rows for `/terminals`: pick one background terminal to
/// terminate, or clean up every finished one. Callers pass only the controls
/// the installed Codex bundle actually supports.
#[must_use]
pub fn terminal_rows(
    terminals: impl IntoIterator<Item = (String, String, String)>,
    can_clean: bool,
) -> Vec<CreateActionRow> {
    let options: Vec<_> = terminals
        .into_iter()
        .take(25)
        .map(|(process_id, command, detail)| {
            CreateSelectMenuOption::new(truncate(&command, 100), process_id)
                .description(truncate(&detail, 100))
        })
        .collect();
    let mut rows = Vec::new();
    if !options.is_empty() {
        rows.push(CreateActionRow::SelectMenu(
            CreateSelectMenu::new(TERMINAL_KILL, CreateSelectMenuKind::String { options })
                .placeholder("Terminate a background terminal")
                .min_values(1)
                .max_values(1),
        ));
    }
    if can_clean {
        rows.push(CreateActionRow::Buttons(vec![
            CreateButton::new(TERMINAL_CLEAN)
                .label("Clean finished")
                .emoji('🧹')
                .style(ButtonStyle::Secondary),
        ]));
    }
    rows
}

/// Collaboration-mode preset picker for `/mode`.
#[must_use]
pub fn mode_select(presets: impl IntoIterator<Item = (String, String)>) -> Vec<CreateActionRow> {
    let options = presets
        .into_iter()
        .take(25)
        .map(|(name, description)| {
            CreateSelectMenuOption::new(truncate(&name, 100), name.clone())
                .description(truncate(&description, 100))
        })
        .collect();
    vec![CreateActionRow::SelectMenu(
        CreateSelectMenu::new(MODE_SELECT, CreateSelectMenuKind::String { options })
            .placeholder("Choose a collaboration mode for new turns")
            .min_values(1)
            .max_values(1),
    )]
}

pub fn task_browser_navigation(token: &str, has_previous: bool, has_next: bool) -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{TASK_BROWSER_PAGE}:{token}:prev"))
            .label("Previous")
            .style(ButtonStyle::Secondary)
            .disabled(!has_previous),
        CreateButton::new(format!("{TASK_BROWSER_PAGE}:{token}:next"))
            .label("Next")
            .style(ButtonStyle::Primary)
            .disabled(!has_next),
    ])
}

#[must_use]
pub fn new_task_inputs() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Paragraph, "What should Codex do?", "prompt")
                .placeholder("Describe complete outcome…")
                .required(true)
                .max_length(4000),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Short,
                "Working directory (absolute path)",
                "cwd",
            )
            .placeholder("C:\\path\\to\\project")
            .required(false)
            .max_length(500),
        ),
    ]
}

#[must_use]
pub fn god_password_input() -> Vec<CreateActionRow> {
    vec![CreateActionRow::InputText(
        CreateInputText::new(InputTextStyle::Short, "GOD-mode password", "password")
            .placeholder("Secret is never stored or logged")
            .required(true)
            .max_length(MAX_PASSWORD_LENGTH as u16),
    )]
}

#[must_use]
pub fn continue_input() -> Vec<CreateActionRow> {
    vec![CreateActionRow::InputText(
        CreateInputText::new(InputTextStyle::Paragraph, "Message to Codex", "prompt")
            .placeholder("Continue, steer, ask a question, or give new instructions…")
            .required(true)
            .max_length(4000),
    )]
}

#[must_use]
pub fn email_inputs() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "To", "to")
                .placeholder("person@example.com")
                .required(true)
                .max_length(320),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "Subject", "subject")
                .required(true)
                .max_length(998),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Short, "CC (optional)", "cc")
                .required(false)
                .max_length(2000),
        ),
        CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Paragraph, "Message", "body")
                .placeholder("Write the email body…")
                .required(true)
                .max_length(4000),
        ),
    ]
}

#[must_use]
pub fn server_answer_inputs(
    fields: impl IntoIterator<Item = (String, String, String, bool)>,
) -> Vec<CreateActionRow> {
    fields
        .into_iter()
        .take(5)
        .map(|(id, label, placeholder, required)| {
            CreateActionRow::InputText(
                CreateInputText::new(
                    InputTextStyle::Paragraph,
                    truncate(&label, 45),
                    truncate(&id, 100),
                )
                .placeholder(truncate(&placeholder, 100))
                .required(required)
                .max_length(4000),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_button_serializes_a_discord_unicode_emoji() {
        let rows = serde_json::to_value(task_buttons(false)).unwrap();

        assert_eq!(rows[1]["components"][0]["emoji"]["name"], "🔀");
    }

    #[test]
    fn custom_id_arg_requires_the_exact_prefix_and_separator() {
        assert_eq!(
            custom_id_arg("relay:action_page:cat:2", ACTION_PAGE),
            Some("cat:2")
        );
        assert_eq!(custom_id_arg("relay:action_page", ACTION_PAGE), None);
        assert_eq!(custom_id_arg("relay:action_pagex:1", ACTION_PAGE), None);
        assert_eq!(custom_id_arg("other:action_page:1", ACTION_PAGE), None);
    }

    #[test]
    fn typed_input_picker_keeps_capability_data_out_of_discord_ids() {
        let token = "0123456789abcdef0123456789abcdef";
        let rows = serde_json::to_value(typed_input_select(
            token,
            "skill",
            [(0, "Review\nunsafe".to_owned(), "Review\rcode".to_owned())],
        ))
        .unwrap();
        let custom_id = rows[0]["components"][0]["custom_id"].as_str().unwrap();
        assert_eq!(custom_id, format!("{TYPED_INPUT_SELECT}:{token}"));
        assert!(custom_id.len() <= 100);
        assert_eq!(rows[0]["components"][0]["options"][0]["value"], "0");
        assert!(!custom_id.contains("Review"));
        assert_eq!(
            rows[0]["components"][0]["options"][0]["label"],
            "Review unsafe"
        );
    }

    #[test]
    fn realtime_controls_match_session_lifecycle() {
        let active = serde_json::to_value(realtime_buttons(true)).unwrap();
        assert_eq!(active[0]["components"][0]["custom_id"], REALTIME_TEXT);
        assert_eq!(active[0]["components"][1]["custom_id"], REALTIME_VOICES);
        assert_eq!(active[0]["components"][2]["custom_id"], REALTIME_STOP);

        let inactive = serde_json::to_value(realtime_buttons(false)).unwrap();
        assert_eq!(inactive[0]["components"][0]["custom_id"], REALTIME_START);
        assert_eq!(inactive[0]["components"][1]["custom_id"], REALTIME_VOICES);
        assert_eq!(inactive[0]["components"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn plugin_controls_use_opaque_indices_and_safe_auth_links() {
        let token = "0123456789abcdef0123456789abcdef";
        let rows = serde_json::to_value(plugin_browser_controls(
            token,
            [(42, "Gmail".to_owned(), "official · email".to_owned())],
            1,
            3,
        ))
        .unwrap();
        assert_eq!(
            rows[0]["components"][0]["custom_id"],
            format!("{PLUGIN_SELECT}:{token}")
        );
        assert_eq!(rows[0]["components"][0]["options"][0]["value"], "42");

        let auth = serde_json::to_value(plugin_auth_buttons([
            ("Gmail".to_owned(), "https://example.test/auth".to_owned()),
            ("Unsafe".to_owned(), "file:///secret".to_owned()),
        ]))
        .unwrap();
        assert_eq!(auth.as_array().unwrap().len(), 1);
        assert_eq!(auth[0]["components"].as_array().unwrap().len(), 1);
        assert_eq!(auth[0]["components"][0]["url"], "https://example.test/auth");
    }

    #[test]
    fn setup_link_buttons_reject_everything_but_https() {
        let rows = serde_json::to_value(connector_link_buttons([
            (
                "Gmail".to_owned(),
                "https://chatgpt.com/apps/gmail".to_owned(),
            ),
            ("Evil".to_owned(), "javascript:alert(1)".to_owned()),
            (
                "Plain".to_owned(),
                "http://chatgpt.com/apps/plain".to_owned(),
            ),
        ]))
        .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["components"].as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["components"][0]["label"], "Install Gmail");
        assert_eq!(
            rows[0]["components"][0]["url"],
            "https://chatgpt.com/apps/gmail"
        );

        // The auth variant shares the same https-only filter.
        let auth = serde_json::to_value(plugin_auth_buttons([(
            "Plain".to_owned(),
            "http://example.test/auth".to_owned(),
        )]))
        .unwrap();
        assert!(auth.as_array().unwrap().is_empty());
    }

    #[test]
    fn secure_links_reject_credentials_and_plain_http() {
        assert_eq!(
            safe_https_url("https://example.test/oauth"),
            Some("https://example.test/oauth".to_owned())
        );
        assert!(safe_https_url("http://example.test/oauth").is_none());
        assert!(safe_https_url("https://user:secret@example.test/oauth").is_none());
        assert!(safe_https_url("javascript:alert(1)").is_none());

        let controls = serde_json::to_value(elicitation_url_buttons(
            "request-1",
            Some("http://example.test/oauth"),
        ))
        .unwrap();
        assert!(controls.to_string().contains("I completed it"));
        assert!(!controls.to_string().contains("http://"));
    }
}
