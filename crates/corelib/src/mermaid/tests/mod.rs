use super::*;

/// One small, real example of every diagram type mermaid ships.
const GALLERY: &[(&str, &str)] = &[
    ("flowchart", "flowchart TD\n A[Start] --> B{Ok?}\n B -->|yes| C([Done])"),
    ("graph", "graph LR\n A --> B"),
    ("sequenceDiagram", "sequenceDiagram\n actor U as You\n U->>+S: hi\n S-->>-U: hello\n Note over U,S: paired"),
    ("classDiagram", "classDiagram\n class A {\n +int x\n }\n A <|-- B"),
    ("stateDiagram-v2", "stateDiagram-v2\n [*] --> Idle\n Idle --> Busy : go\n Busy --> [*]"),
    ("erDiagram", "erDiagram\n CUSTOMER ||--o{ ORDER : places"),
    ("journey", "journey\n title Day\n section Work\n  Code: 5: Me"),
    ("gantt", "gantt\n dateFormat YYYY-MM-DD\n section S\n A :a1, 2024-01-01, 5d"),
    ("pie", "pie title Pets\n \"Dogs\" : 3\n \"Cats\" : 1"),
    ("quadrantChart", "quadrantChart\n title Reach\n quadrant-1 Expand\n A: [0.4, 0.6]"),
    ("requirementDiagram", "requirementDiagram\n requirement r {\n id: 1\n }\n element e {\n type: sim\n }\n e - satisfies -> r"),
    ("gitGraph", "gitGraph\n commit\n branch dev\n commit\n checkout main\n merge dev"),
    ("C4Context", "C4Context\n title Sys\n Person(a, \"User\")\n System(b, \"App\")\n Rel(a, b, \"uses\")"),
    ("mindmap", "mindmap\n  root((Root))\n    One\n    Two"),
    ("timeline", "timeline\n title T\n 2002 : One : Two"),
    ("kanban", "kanban\n  Todo\n    [Write]\n  Doing\n    [Review]"),
    ("sankey-beta", "sankey-beta\n A,B,10\n A,C,5"),
    ("xychart-beta", "xychart-beta\n x-axis [a, b]\n bar [1, 2]"),
    ("block-beta", "block-beta\n columns 2\n a b\n a --> b"),
    ("packet-beta", "packet-beta\n 0-15: \"Source\""),
    ("architecture-beta", "architecture-beta\n group g(cloud)[API]\n service db(database)[DB] in g"),
    ("radar-beta", "radar-beta\n axis a[\"A\"], b[\"B\"], c[\"C\"]\n curve me[\"Me\"]{1, 2, 3}"),
    ("treemap-beta", "treemap-beta\n \"Sec\"\n  \"Leaf\": 5"),
    ("info", "info\n showInfo"),
];

#[test]
fn every_diagram_type_parses_lays_out_and_draws() {
    for (name, src) in GALLERY {
        let d = parse(src).unwrap_or_else(|| panic!("{name} does not parse"));
        let px = layout(&d, &|s: &str| (crate::unicode::str_width(s) as u32 * 8, 16));
        assert!(px.width > 0 && px.height > 0, "{name} lays out to nothing");
        assert!(!px.items.is_empty(), "{name} draws nothing");
        let rows = art(src, 200).unwrap_or_else(|| panic!("{name} does not draw as text"));
        assert!(rows.iter().any(|r| !r.trim().is_empty()), "{name} draws blank rows");
    }
}

#[test]
fn no_diagram_type_leaks_its_own_syntax_into_the_picture() {
    // The promise the AI prompt makes: the user sees a picture, never the source.
    for (name, src) in GALLERY {
        let drawn = art(src, 200).unwrap_or_default().join("\n");
        for jargon in ["-->", "```", "mermaid", "|--", "::"] {
            assert!(!drawn.contains(jargon), "{name} leaked {jargon:?}:\n{drawn}");
        }
    }
}

#[test]
fn hostile_and_truncated_sources_never_panic() {
    for (_, src) in GALLERY {
        // Every prefix of every example — what a streaming model sends mid-answer.
        for cut in [1, 5, 12, 30] {
            let partial: String = src.chars().take(cut).collect();
            if let Some(d) = parse(&partial) {
                let _ = layout(&d, &|s: &str| (s.len() as u32, 1));
            }
        }
    }
    for junk in ["flowchart TD\n {{{{", "pie\n \"a\" : not-a-number", "gantt\n x :,,,,", "erDiagram\n ||--||", "mindmap\n\t\t\t"] {
        if let Some(d) = parse(junk) {
            let _ = layout(&d, &|s: &str| (s.len() as u32, 1));
        }
    }
}
