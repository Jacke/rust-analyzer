use std::{iter, ops::RangeInclusive};

use arrayvec::ArrayVec;
use hir::Name;
use ra_fmt::leading_indent;
use ra_syntax::{
    ast, AstNode, Direction, InsertPosition, SourceFile, SyntaxElement, SyntaxKind::*, T,
};
use ra_text_edit::TextEditBuilder;

pub struct AstEditor<N: AstNode> {
    original_ast: N,
    ast: N,
}

impl<N: AstNode> AstEditor<N> {
    pub fn new(node: N) -> AstEditor<N> {
        AstEditor { original_ast: node.clone(), ast: node }
    }

    pub fn into_text_edit(self, builder: &mut TextEditBuilder) {
        // FIXME: compute a more fine-grained diff here.
        // If *you* know a nice algorithm to compute diff between two syntax
        // tree, tell me about it!
        builder.replace(
            self.original_ast.syntax().text_range(),
            self.ast().syntax().text().to_string(),
        );
    }

    pub fn ast(&self) -> &N {
        &self.ast
    }

    #[must_use]
    fn insert_children(
        &self,
        position: InsertPosition<SyntaxElement>,
        to_insert: impl Iterator<Item = SyntaxElement>,
    ) -> N {
        let new_syntax = self.ast().syntax().insert_children(position, to_insert);
        N::cast(new_syntax).unwrap()
    }

    #[must_use]
    fn replace_children(
        &self,
        to_delete: RangeInclusive<SyntaxElement>,
        to_insert: impl Iterator<Item = SyntaxElement>,
    ) -> N {
        let new_syntax = self.ast().syntax().replace_children(to_delete, to_insert);
        N::cast(new_syntax).unwrap()
    }

    fn do_make_multiline(&mut self) {
        let l_curly =
            match self.ast().syntax().children_with_tokens().find(|it| it.kind() == T!['{']) {
                Some(it) => it,
                None => return,
            };
        let sibling = match l_curly.next_sibling_or_token() {
            Some(it) => it,
            None => return,
        };
        let existing_ws = match sibling.as_token() {
            None => None,
            Some(tok) if tok.kind() != WHITESPACE => None,
            Some(ws) => {
                if ws.text().contains('\n') {
                    return;
                }
                Some(ws.clone())
            }
        };

        let indent = leading_indent(self.ast().syntax()).unwrap_or("".into());
        let ws = tokens::WsBuilder::new(&format!("\n{}", indent));
        let to_insert = iter::once(ws.ws().into());
        self.ast = match existing_ws {
            None => self.insert_children(InsertPosition::After(l_curly), to_insert),
            Some(ws) => {
                self.replace_children(RangeInclusive::new(ws.clone().into(), ws.into()), to_insert)
            }
        };
    }
}

impl AstEditor<ast::NamedFieldList> {
    pub fn append_field(&mut self, field: &ast::NamedField) {
        self.insert_field(InsertPosition::Last, field)
    }

    pub fn insert_field(
        &mut self,
        position: InsertPosition<&'_ ast::NamedField>,
        field: &ast::NamedField,
    ) {
        let is_multiline = self.ast().syntax().text().contains('\n');
        let ws;
        let space = if is_multiline {
            ws = tokens::WsBuilder::new(&format!(
                "\n{}    ",
                leading_indent(self.ast().syntax()).unwrap_or("".into())
            ));
            ws.ws()
        } else {
            tokens::single_space()
        };

        let mut to_insert: ArrayVec<[SyntaxElement; 4]> = ArrayVec::new();
        to_insert.push(space.into());
        to_insert.push(field.syntax().clone().into());
        to_insert.push(tokens::comma().into());

        macro_rules! after_l_curly {
            () => {{
                let anchor = match self.l_curly() {
                    Some(it) => it,
                    None => return,
                };
                InsertPosition::After(anchor)
            }};
        }

        macro_rules! after_field {
            ($anchor:expr) => {
                if let Some(comma) = $anchor
                    .syntax()
                    .siblings_with_tokens(Direction::Next)
                    .find(|it| it.kind() == T![,])
                {
                    InsertPosition::After(comma)
                } else {
                    to_insert.insert(0, tokens::comma().into());
                    InsertPosition::After($anchor.syntax().clone().into())
                }
            };
        };

        let position = match position {
            InsertPosition::First => after_l_curly!(),
            InsertPosition::Last => {
                if !is_multiline {
                    // don't insert comma before curly
                    to_insert.pop();
                }
                match self.ast().fields().last() {
                    Some(it) => after_field!(it),
                    None => after_l_curly!(),
                }
            }
            InsertPosition::Before(anchor) => {
                InsertPosition::Before(anchor.syntax().clone().into())
            }
            InsertPosition::After(anchor) => after_field!(anchor),
        };

        self.ast = self.insert_children(position, to_insert.iter().cloned());
    }

    fn l_curly(&self) -> Option<SyntaxElement> {
        self.ast().syntax().children_with_tokens().find(|it| it.kind() == T!['{'])
    }
}

impl AstEditor<ast::ItemList> {
    pub fn append_items(&mut self, items: impl Iterator<Item = ast::ImplItem>) {
        let n_existing_items = self.ast().impl_items().count();
        if n_existing_items == 0 {
            self.do_make_multiline();
        }
        items.for_each(|it| self.append_item(it));
    }

    pub fn append_item(&mut self, item: ast::ImplItem) {
        let (indent, position) = match self.ast().impl_items().last() {
            Some(it) => (
                leading_indent(it.syntax()).unwrap_or_default().to_string(),
                InsertPosition::After(it.syntax().clone().into()),
            ),
            None => match self.l_curly() {
                Some(it) => (
                    "    ".to_string() + &leading_indent(self.ast().syntax()).unwrap_or_default(),
                    InsertPosition::After(it),
                ),
                None => return,
            },
        };
        let ws = tokens::WsBuilder::new(&format!("\n{}", indent));
        let to_insert: ArrayVec<[SyntaxElement; 2]> =
            [ws.ws().into(), item.syntax().clone().into()].into();
        self.ast = self.insert_children(position, to_insert.into_iter());
    }

    fn l_curly(&self) -> Option<SyntaxElement> {
        self.ast().syntax().children_with_tokens().find(|it| it.kind() == T!['{'])
    }
}

impl AstEditor<ast::ImplItem> {
    pub fn strip_attrs_and_docs(&mut self) {
        while let Some(start) = self
            .ast()
            .syntax()
            .children_with_tokens()
            .find(|it| it.kind() == ATTR || it.kind() == COMMENT)
        {
            let end = match &start.next_sibling_or_token() {
                Some(el) if el.kind() == WHITESPACE => el.clone(),
                Some(_) | None => start.clone(),
            };
            self.ast = self.replace_children(RangeInclusive::new(start, end), iter::empty());
        }
    }
}

impl AstEditor<ast::FnDef> {
    pub fn set_body(&mut self, body: &ast::Block) {
        let mut to_insert: ArrayVec<[SyntaxElement; 2]> = ArrayVec::new();
        let old_body_or_semi: SyntaxElement = if let Some(old_body) = self.ast().body() {
            old_body.syntax().clone().into()
        } else if let Some(semi) = self.ast().semicolon_token() {
            to_insert.push(tokens::single_space().into());
            semi.into()
        } else {
            to_insert.push(tokens::single_space().into());
            to_insert.push(body.syntax().clone().into());
            self.ast = self.insert_children(InsertPosition::Last, to_insert.into_iter());
            return;
        };
        to_insert.push(body.syntax().clone().into());
        let replace_range = RangeInclusive::new(old_body_or_semi.clone(), old_body_or_semi);
        self.ast = self.replace_children(replace_range, to_insert.into_iter())
    }
}

pub struct AstBuilder<N: AstNode> {
    _phantom: std::marker::PhantomData<N>,
}

impl AstBuilder<ast::NamedField> {
    pub fn from_name(name: &Name) -> ast::NamedField {
        ast_node_from_file_text(&format!("fn f() {{ S {{ {}: (), }} }}", name))
    }

    fn from_text(text: &str) -> ast::NamedField {
        ast_node_from_file_text(&format!("fn f() {{ S {{ {}, }} }}", text))
    }

    pub fn from_pieces(name: &ast::NameRef, expr: Option<&ast::Expr>) -> ast::NamedField {
        match expr {
            Some(expr) => Self::from_text(&format!("{}: {}", name.syntax(), expr.syntax())),
            None => Self::from_text(&name.syntax().to_string()),
        }
    }
}

impl AstBuilder<ast::Block> {
    fn from_text(text: &str) -> ast::Block {
        ast_node_from_file_text(&format!("fn f() {}", text))
    }

    pub fn single_expr(e: &ast::Expr) -> ast::Block {
        Self::from_text(&format!("{{ {} }}", e.syntax()))
    }
}

impl AstBuilder<ast::Expr> {
    fn from_text(text: &str) -> ast::Expr {
        ast_node_from_file_text(&format!("fn f() {{ {}; }}", text))
    }

    pub fn unit() -> ast::Expr {
        Self::from_text("()")
    }

    pub fn unimplemented() -> ast::Expr {
        Self::from_text("unimplemented!()")
    }
}

impl AstBuilder<ast::NameRef> {
    pub fn new(text: &str) -> ast::NameRef {
        ast_node_from_file_text(&format!("fn f() {{ {}; }}", text))
    }
}

fn ast_node_from_file_text<N: AstNode>(text: &str) -> N {
    let parse = SourceFile::parse(text);
    let res = parse.tree().syntax().descendants().find_map(N::cast).unwrap().to_owned();
    res
}

mod tokens {
    use once_cell::sync::Lazy;
    use ra_syntax::{AstNode, Parse, SourceFile, SyntaxKind::*, SyntaxToken, T};

    static SOURCE_FILE: Lazy<Parse<SourceFile>> = Lazy::new(|| SourceFile::parse(",\n; ;"));

    pub(crate) fn comma() -> SyntaxToken {
        SOURCE_FILE
            .tree()
            .syntax()
            .descendants_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|it| it.kind() == T![,])
            .unwrap()
    }

    pub(crate) fn single_space() -> SyntaxToken {
        SOURCE_FILE
            .tree()
            .syntax()
            .descendants_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|it| it.kind() == WHITESPACE && it.text().as_str() == " ")
            .unwrap()
    }

    #[allow(unused)]
    pub(crate) fn single_newline() -> SyntaxToken {
        SOURCE_FILE
            .tree()
            .syntax()
            .descendants_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|it| it.kind() == WHITESPACE && it.text().as_str() == "\n")
            .unwrap()
    }

    pub(crate) struct WsBuilder(SourceFile);

    impl WsBuilder {
        pub(crate) fn new(text: &str) -> WsBuilder {
            WsBuilder(SourceFile::parse(text).ok().unwrap())
        }
        pub(crate) fn ws(&self) -> SyntaxToken {
            self.0.syntax().first_child_or_token().unwrap().into_token().unwrap()
        }
    }

}
