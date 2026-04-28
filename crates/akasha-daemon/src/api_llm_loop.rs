pub(crate) fn should_run_deterministic_preferred(
    strict_mode_active: bool,
    strict_preferred_tool_replay_input_present: bool,
    deterministic_preferred_attempted: bool,
    round: u32,
) -> bool {
    strict_mode_active
        && (strict_preferred_tool_replay_input_present
            || (!deterministic_preferred_attempted && round == 0))
}
