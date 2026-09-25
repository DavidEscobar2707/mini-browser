//! Page affordances: search boxes, fake search triggers, label similarity,
//! and when a page really needs a scroll-through. Contract: plans/007.

use crate::qa::ObservedElement;

/// Roles that name a text-entry control, even one the page fakes: exito's and
/// ktronix's search boxes are `role=textbox` elements that cannot be typed
/// into until they are clicked.
const SEARCH_ROLES: [&str; 3] = ["searchbox", "combobox", "textbox"];

/// Words that make a label search-like. A token counts when it starts with the
/// word, so "buscar" also covers "búsqueda"/"buscador" and "search" covers
/// "searching" — every word here is at least 4 characters, so no short token
/// matches by accident.
const SEARCH_WORDS: [&str; 7] = ["search", "buscar", "busca", "buscas", "find", "query", "lupa"];

/// Tokens that carry no identity of their own: a drop-down label ends in
/// "icon: down", and a button label may end in "button".
const STOP_TOKENS: [&str; 4] = ["icon", "down", "up", "button"];

/// Shortest token that may be the same token as a longer one by prefix
/// ("teatro"/"teatros").
const MIN_PREFIX: usize = 4;

/// Below this the goal check says the page does not show what the goal asks
/// for, so content hidden below the fold is worth loading. (`move_cmd`'s
/// `GOAL_MET_AT` is the other verdict: above it no action is needed at all.)
const GOAL_MET_OK: f64 = 0.5;

/// Above this ratio of full-page text to visible text the DOM already holds
/// more than the screen shows, so lazy content has rendered.
const LAZY_RATIO: f64 = 1.2;

/// True for a control that takes search text: by role, by `input type=search`,
/// or by the wording of its label or name. False for a plain link whose label
/// is only a category ("Celulares").
pub fn is_search_like(el: &ObservedElement) -> bool {
    if SEARCH_ROLES.contains(&fold(el.role.as_deref().unwrap_or("")).as_str()) {
        return true;
    }
    if el.input_type.as_deref().map(|t| fold(t) == "search").unwrap_or(false) {
        return true;
    }
    let named = el.name.as_deref().map(search_wording).unwrap_or(false);
    named || search_wording(&el.label)
}

/// A fake search box: a search-like control whose `kind` is `click`, so the
/// click opens an overlay holding the real field, or a button that says it
/// opens the search. The Amazon submit button ("Search Amazon") is neither:
/// it is what a real field is submitted with.
pub fn is_fake_search_trigger(el: &ObservedElement) -> bool {
    if fold(&el.kind) != "click" {
        return false;
    }
    let role = fold(el.role.as_deref().unwrap_or(""));
    if SEARCH_ROLES.contains(&role.as_str()) {
        return is_search_like(el);
    }
    role == "button" && opens_search(&el.label)
}

/// True when the goal asks for a search, so a page that shows no search box
/// can be helped by opening the overlay one.
pub fn goal_wants_search(goal: &str) -> bool {
    let goal = fold(goal);
    goal.contains("look up") || SEARCH_WORDS.iter().any(|word| goal.contains(word))
}

/// Token Jaccard: `|A ∩ B| / |A ∪ B|` over the folded tokens of both labels,
/// stop tokens dropped. Tokens that are one word in two spellings are merged
/// first — equal, or the shorter one (at least [`MIN_PREFIX`] characters) a
/// prefix of the longer — so a drop-down's "teatro" and "teatros" are one
/// token and not a difference. The pair 006's replay broke on, "Seleccionar
/// Teatro Todos los teatros icon: down" against "Seleccionar Teatro Arkadia
/// icon: down", is 2 of 5 (0.4); the same drop-down reopened with its own
/// selection in the label, "Seleccionar Teatro icon: down" against
/// "Seleccionar Teatro Arkadia icon: down", is 2 of 3 (0.67) and so clears the
/// 0.5 replay pick.
pub fn label_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let (a, b) = (content_tokens(a), content_tokens(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let all: Vec<&str> = a.iter().chain(b.iter()).map(String::as_str).collect();
    let mut parent: Vec<usize> = (0..all.len()).collect();
    for i in 0..all.len() {
        for j in i + 1..all.len() {
            if same_token(all[i], all[j]) {
                union(&mut parent, i, j);
            }
        }
    }
    let a_ids = cluster_ids(&mut parent, a.len(), 0);
    let b_ids = cluster_ids(&mut parent, b.len(), a.len());
    let shared = a_ids.iter().filter(|id| b_ids.contains(id)).count();
    shared as f64 / (a_ids.len() + b_ids.len() - shared) as f64
}

/// True when the page still has content to render: a line that is nothing but
/// a loading word, or a DOM no bigger than the screen while the goal is not
/// met, both mean scrolling may pull the rest of the page in.
pub fn needs_scroll_through(visible_text: &str, full_text: &str, goal_met: Option<f64>) -> bool {
    // The same loading-word set `move_cmd::drop_loading_lines` drops from the
    // report: a line it changes is a placeholder.
    if crate::move_cmd::drop_loading_lines(visible_text) != visible_text {
        return true;
    }
    let visible = visible_text.chars().count() as f64;
    let full = full_text.chars().count() as f64;
    full < visible * LAZY_RATIO && goal_met.unwrap_or(0.0) < GOAL_MET_OK
}

// ---- words ----

/// Lowercased and accent-free, so "Búsqueda" and "busqueda" are one string:
/// sites and goals spell the same word differently.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'ç' => 'c',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            other => other,
        })
        .collect()
}

/// The folded alphanumeric runs of `text`, in order, duplicates kept: the
/// similarity clusters need every occurrence.
fn tokens(text: &str) -> Vec<String> {
    fold(text)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn content_tokens(text: &str) -> Vec<String> {
    tokens(text).into_iter().filter(|t| !STOP_TOKENS.contains(&t.as_str())).collect()
}

/// The wording rule on its own: a token that starts with a search word.
/// "Celulares" has none; "Search Amazon" and "¿Qué buscas hoy?" have one.
fn search_wording(text: &str) -> bool {
    tokens(text).iter().any(|token| SEARCH_WORDS.iter().any(|word| token.starts_with(word)))
}

/// "Open search" / "Abrir búsqueda" / "Buscar", but not a label that merely
/// contains the word search ("Search Amazon" is the submit button).
fn opens_search(label: &str) -> bool {
    let label = fold(label);
    label.contains("open search") || label.contains("abrir busqueda") || label.contains("buscar")
}

fn same_token(a: &str, b: &str) -> bool {
    a == b || (a.len() >= MIN_PREFIX && b.starts_with(a)) || (b.len() >= MIN_PREFIX && a.starts_with(b))
}

/// One word spelled two ways is one token: `parent` unions the token indices
/// [`same_token`] says are the same, and this returns the clusters of one
/// token list as a set, so a duplicate counts once.
fn cluster_ids(parent: &mut [usize], len: usize, offset: usize) -> Vec<usize> {
    let mut ids = Vec::new();
    for i in 0..len {
        let id = find(parent, offset + i);
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    let mut root = i;
    while parent[root] != root {
        root = parent[root];
    }
    while parent[i] != root {
        let next = parent[i];
        parent[i] = root;
        i = next;
    }
    root
}

fn union(parent: &mut [usize], i: usize, j: usize) {
    let (ri, rj) = (find(parent, i), find(parent, j));
    if ri != rj {
        parent[rj] = ri;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(kind: &str, role: Option<&str>, label: &str) -> ObservedElement {
        ObservedElement {
            kind: kind.into(),
            role: role.map(str::to_string),
            label: label.into(),
            ..Default::default()
        }
    }

    fn input(kind: &str, role: Option<&str>, label: &str, input_type: &str) -> ObservedElement {
        ObservedElement {
            input_type: Some(input_type.into()),
            ..el(kind, role, label)
        }
    }

    #[test]
    fn exito_fake_textbox_opens_the_search() {
        let field = input("fill", Some("textbox"), "search", "text");
        assert!(is_search_like(&field), "the real field behind the overlay");
        assert!(!is_fake_search_trigger(&field), "a field that takes text is not a trigger");

        let trigger = input("click", Some("textbox"), "Open search", "text");
        assert!(is_search_like(&trigger));
        assert!(is_fake_search_trigger(&trigger), "the fake textbox opens the overlay");
    }

    #[test]
    fn ktronix_placeholder_textbox_is_a_trigger() {
        let trigger = el("click", Some("textbox"), "¿Qué buscas hoy?");
        assert!(is_search_like(&trigger), "the wording carries it: no input type, no name");
        assert!(is_fake_search_trigger(&trigger));
    }

    #[test]
    fn amazon_search_field_and_submit_are_real() {
        let field = input("fill", Some("textbox"), "Search Amazon", "search");
        assert!(is_search_like(&field));
        assert!(!is_fake_search_trigger(&field));

        let submit = el("click", Some("button"), "Search Amazon");
        assert!(is_search_like(&submit), "the label says what it does");
        assert!(!is_fake_search_trigger(&submit), "a real submit is not an overlay opener");

        let opener = el("click", Some("button"), "Abrir búsqueda");
        assert!(is_fake_search_trigger(&opener), "the button wording alone is enough");

        let by_name = ObservedElement {
            name: Some("search".into()),
            ..el("fill", None, "")
        };
        assert!(is_search_like(&by_name), "a bare field is found by its name");
        assert!(!is_fake_search_trigger(&by_name));
    }

    #[test]
    fn a_category_link_is_not_a_search_box() {
        let link = el("click", Some("link"), "Celulares");
        assert!(!is_search_like(&link));
        assert!(!is_fake_search_trigger(&link));

        // The role is what a search box is built from, so a combobox is
        // search-like whoever labels it; a select that is not clicked is not a
        // trigger.
        let select = el("select", Some("combobox"), "Departamento");
        assert!(is_search_like(&select));
        assert!(!is_fake_search_trigger(&select));
    }

    #[test]
    fn the_dropdown_pair_of_006_is_similar() {
        let all = "Seleccionar Teatro Todos los teatros icon: down";
        let one = "Seleccionar Teatro Arkadia icon: down";
        let alike = label_similarity(all, one);
        assert!(alike >= 0.4, "plans/007 asks for 0.4, got {alike}");
        assert!(
            label_similarity("Seleccionar Teatro icon: down", one) >= 0.5,
            "the same drop-down reopened with a selection in its label clears the replay pick"
        );
        assert_eq!(label_similarity(one, one), 1.0, "equal strings are equal");
        assert_eq!(label_similarity(one, "   "), 0.0, "no tokens, no similarity");
        assert_eq!(label_similarity("Parking", "Book"), 0.0);
        assert!(
            label_similarity(one, "Seleccionar Ciudad Bogotá icon: down") < 0.4,
            "another drop-down of the same page stays apart"
        );
    }

    #[test]
    fn the_goal_says_whether_it_wants_a_search() {
        assert!(goal_wants_search("Buscar boletas para Dune"));
        assert!(goal_wants_search("Search flights to Bogotá"));
        assert!(goal_wants_search("find a table for two"));
        assert!(goal_wants_search("Look up the showtimes"));
        assert!(!goal_wants_search("comprar un celular"));
        assert!(!goal_wants_search("book a table for two"));
    }

    #[test]
    fn a_loading_line_means_the_content_is_still_coming() {
        let placeholder = "Cartelera\nCargando\u{2026}\n";
        assert!(
            needs_scroll_through(placeholder, placeholder, Some(0.9)),
            "not even a passing goal check fixes a placeholder"
        );
        let sentence = "Cargando la cartelera de hoy";
        assert!(
            !needs_scroll_through(sentence, &sentence.repeat(3), Some(0.9)),
            "the loading word inside a sentence is content"
        );
    }

    #[test]
    fn a_dom_no_bigger_than_the_screen_is_worth_scrolling() {
        let visible = "Cartelera\nPoseido 19:00";
        let rendered = "Cartelera\nPoseido 19:00\nDia Ocho 20:40\nDune 21:00";
        assert!(!needs_scroll_through(visible, rendered, None), "the rest of the page already rendered");
        assert!(needs_scroll_through(visible, visible, None), "nothing below the fold is in the DOM yet");
        assert!(needs_scroll_through(visible, visible, Some(0.2)), "and the goal is not met here");
        assert!(!needs_scroll_through(visible, visible, Some(0.8)), "the goal check says the page is enough");
    }

    #[test]
    fn the_lazy_ratio_boundary_is_exclusive() {
        let visible = "0123456789";
        assert!(
            !needs_scroll_through(visible, "0123456789ab", None),
            "12 characters is exactly 1.2x: the DOM grew, nothing lazy is missing"
        );
        assert!(needs_scroll_through(visible, "0123456789a", None), "11 is below 1.2x");
    }
}
