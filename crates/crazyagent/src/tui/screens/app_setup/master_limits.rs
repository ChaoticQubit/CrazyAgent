use super::*;

/// The title of one master step limit option and what it does, the same words as the
/// desktop Settings. Input: the option. Output: the title and the sentence.
pub(in crate::tui) fn step_limit_words(limit: StepLimit) -> (&'static str, &'static str) {
    match limit {
        StepLimit::NoLimit => (
            "No limit",
            "A turn runs until the master replies or its time is up.",
        ),
        StepLimit::Steps50 => ("50 steps", "A turn stops after 50 steps."),
        StepLimit::Steps100 => ("100 steps", "A turn stops after 100 steps."),
        StepLimit::Steps200 => ("200 steps", "A turn stops after 200 steps."),
    }
}

/// The title of one master time limit option and what it does, the same words as the
/// desktop Settings. Input: the option. Output: the title and the sentence.
pub(in crate::tui) fn time_limit_words(limit: TimeLimit) -> (&'static str, &'static str) {
    match limit {
        TimeLimit::NoLimit => (
            "No limit",
            "A turn runs until the master replies or you stop it.",
        ),
        TimeLimit::Minutes10 => ("10 min", "A turn stops after 10 minutes."),
        TimeLimit::Minutes30 => ("30 min", "A turn stops after 30 minutes."),
        TimeLimit::Minutes60 => ("60 min", "A turn stops after 60 minutes."),
    }
}

/// The title of one master frame size limit option and what it is for, the same words as
/// the desktop Settings. Input: the option. Output: the title and the sentence.
pub(in crate::tui) fn frame_size_words(limit: FrameSize) -> (&'static str, &'static str) {
    match limit {
        FrameSize::Kib64 => (
            "64 KiB",
            "The engine's own message size; enough for most turns.",
        ),
        FrameSize::Kib256 => (
            "256 KiB",
            "Room for long instructions and large pasted text.",
        ),
        FrameSize::Mib1 => (
            "1 MiB",
            "The largest; for a turn that still stops at 256 KiB.",
        ),
    }
}
