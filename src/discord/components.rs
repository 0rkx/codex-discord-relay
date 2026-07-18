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
        ]),
    ]
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
    ])]
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
    if let Some(url) = url.filter(|url| {
        (url.starts_with("https://") || url.starts_with("http://")) && url.len() <= 2_048
    }) {
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
}
