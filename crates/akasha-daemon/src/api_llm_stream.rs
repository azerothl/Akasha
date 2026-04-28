pub(crate) fn choose_merged_response_for_tools(response: &str, accumulated: &str) -> String {
    let r = response.trim();
    let a = accumulated.trim();
    let a_has_tools = !a.is_empty() && !crate::api_tool_parser::parse_tool_calls(a).is_empty();
    let r_has_tools = !r.is_empty() && !crate::api_tool_parser::parse_tool_calls(r).is_empty();
    if r.is_empty() && !a.is_empty() {
        a.to_string()
    } else if a_has_tools && !r_has_tools {
        a.to_string()
    } else if !r.is_empty() {
        r.to_string()
    } else {
        a.to_string()
    }
}
