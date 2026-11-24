use super::*;

/// The tools a Responses API request body offers the model: each function tool by
/// name and every other tool by type. Takes the body. Returns them in order.
fn offered_tools(body: &str) -> Vec<String> {
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    body["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("no tools list: {body}"))
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .or_else(|| tool["type"].as_str())
                .unwrap_or_else(|| panic!("unnamed tool: {tool}"))
                .to_owned()
        })
        .collect()
}

#[test]
fn codex_research_offers_its_model_tools_only_with_tools_on() {
    for tools in [true, false] {
        let case = case("A mobile app that stores family recipes.");
        let jobs = ResearchJobs::open(&case.database).unwrap();
        jobs.enqueue("project-a", "design_research", "codex", tools)
            .unwrap();
        let (port, _server, requests) = recording_hanging_endpoint();
        let run = runner(&case, limits(Duration::from_secs(20)))
            .run_once(&project(&case, Some(endpoint(port))))
            .unwrap()
            .unwrap();
        assert_eq!(run.status, ResearchStatus::Failed);
        let requests = requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turn = requests
            .iter()
            .map(|request| String::from_utf8_lossy(request).into_owned())
            .find(|request| request.starts_with("POST /v1/responses "))
            .unwrap_or_else(|| {
                panic!(
                    "codex asked its model nothing in 20 s; {} connections",
                    requests.len()
                )
            });
        let (head, body) = turn.split_once("\r\n\r\n").unwrap();
        let length = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or_else(|| panic!("no content length: {head}"));
        assert_eq!(body.len(), length, "{head}");
        let offered = offered_tools(body);
        if tools {
            assert!(
                offered.iter().any(|tool| tool == "web_search"),
                "{offered:?}"
            );
            assert!(offered.len() > 1, "{offered:?}");
        } else {
            assert_eq!(offered, Vec::<String>::new());
        }
    }
}
