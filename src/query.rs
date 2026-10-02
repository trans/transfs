#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub components: Vec<String>,
    pub doc_view: bool,
    pub doc_name: Option<String>,
}

pub fn parse(path: &str) -> Parsed {
    let mut result = Parsed {
        components: vec![],
        doc_view: false,
        doc_name: None,
    };
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        if segment == "=" {
            result.doc_view = !result.doc_view;
            result.doc_name = None;
        } else if result.doc_view {
            result.doc_name = Some(segment.into());
        } else {
            result.components.extend(
                segment
                    .replace('=', "/")
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    result
}
