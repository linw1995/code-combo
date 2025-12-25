use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub(crate) enum FoldState {
    Collapsed,
    #[default]
    Preview,
    Expanded,
}

impl FoldState {
    pub(crate) fn is_collapsed(self) -> bool {
        matches!(self, Self::Collapsed)
    }

    pub(crate) fn is_preview(self) -> bool {
        matches!(self, Self::Preview)
    }

    pub(crate) fn toggle(self) -> Self {
        match self {
            Self::Collapsed => Self::Expanded,
            Self::Expanded => Self::Collapsed,
            Self::Preview => Self::Preview,
        }
    }

    pub(crate) fn collapse(&mut self) {
        *self = Self::Collapsed;
    }

    pub(crate) fn expand(&mut self) {
        *self = Self::Expanded;
    }

    pub(crate) fn preview(&mut self) {
        *self = Self::Preview;
    }
}
