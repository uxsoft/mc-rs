//! Fixed application menus; labels, shortcuts, and actions share one definition.
use crate::panel::Sort;

#[derive(Clone, Copy)]
pub enum Action {
    Function(u8),
    Sort(Sort),
    Hidden,
    Refresh,
    Goto,
    Find,
    Jobs,
}
pub struct Item {
    pub label: &'static str,
    pub shortcut: &'static str,
    pub action: Action,
}
pub struct Menu {
    pub label: &'static str,
    pub items: &'static [Item],
}
#[derive(Default)]
pub struct State {
    pub category: usize,
    pub cursor: usize,
    pub offset: usize,
}
pub const MENUS: &[Menu] = &[
    Menu {
        label: "File",
        items: &[
            Item {
                label: "Copy…",
                shortcut: "F5",
                action: Action::Function(5),
            },
            Item {
                label: "Move…",
                shortcut: "F6",
                action: Action::Function(6),
            },
            Item {
                label: "Create directory…",
                shortcut: "F7",
                action: Action::Function(7),
            },
            Item {
                label: "Delete…",
                shortcut: "F8",
                action: Action::Function(8),
            },
            Item {
                label: "Background jobs",
                shortcut: "",
                action: Action::Jobs,
            },
            Item {
                label: "Quit",
                shortcut: "F10",
                action: Action::Function(10),
            },
        ],
    },
    Menu {
        label: "View",
        items: &[
            Item {
                label: "View file",
                shortcut: "F3",
                action: Action::Function(3),
            },
            Item {
                label: "Edit file",
                shortcut: "F4",
                action: Action::Function(4),
            },
            Item {
                label: "Sort by name",
                shortcut: "",
                action: Action::Sort(Sort::Name),
            },
            Item {
                label: "Sort by size",
                shortcut: "",
                action: Action::Sort(Sort::Size),
            },
            Item {
                label: "Sort by modified",
                shortcut: "",
                action: Action::Sort(Sort::Modified),
            },
            Item {
                label: "Show hidden files",
                shortcut: "Alt+.",
                action: Action::Hidden,
            },
            Item {
                label: "Refresh",
                shortcut: "Ctrl+R",
                action: Action::Refresh,
            },
        ],
    },
    Menu {
        label: "Go",
        items: &[
            Item {
                label: "Go to directory…",
                shortcut: "Alt+C",
                action: Action::Goto,
            },
            Item {
                label: "Find filename…",
                shortcut: "Alt+?",
                action: Action::Find,
            },
        ],
    },
    Menu {
        label: "Help",
        items: &[Item {
            label: "Keyboard shortcuts",
            shortcut: "F1",
            action: Action::Function(1),
        }],
    },
];
