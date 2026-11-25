use super::*;

impl TryFrom<String> for ChangeSetId {
    type Error = EngineError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ChangeSetId> for String {
    fn from(id: ChangeSetId) -> Self {
        id.0
    }
}
