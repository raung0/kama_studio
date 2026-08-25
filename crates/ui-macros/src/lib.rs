use std::collections::HashSet;

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{
    braced,
    ext::IdentExt,
    parenthesized,
    parse::{Parse, ParseStream},
    parse_macro_input, Error, Expr, FnArg, Ident, ItemImpl, LitStr, Pat, Path, ReceiverKind,
    Result, Token, Type,
};

#[proc_macro]
pub fn ui(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as UiInput);
    match expand_input(input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn ui_component(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as ComponentArgs);
    let item = parse_macro_input!(input as ItemImpl);
    match expand_component_impl(args, item) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

struct UiInput {
    ctx: Expr,
    nodes: Vec<Node>,
}

impl Parse for UiInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let ctx = input.parse()?;
        input.parse::<Token![,]>()?;
        let content;
        braced!(content in input);
        let nodes = parse_nodes(&content)?;
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after UI tree"));
        }
        Ok(Self { ctx, nodes })
    }
}

struct Element {
    kind: Path,
    state: Option<Expr>,
    entries: Vec<Entry>,
}

enum Entry {
    Property(Property),
    Node(Node),
}

struct Property {
    name: Ident,
    value: Option<Expr>,
    component: bool,
}

enum Node {
    Element(Element),
    Rect {
        key: Expr,
        rect: Expr,
        entries: Vec<Entry>,
    },
    If {
        condition: Expr,
        then_nodes: Vec<Self>,
        else_branch: Option<Box<Self>>,
    },
    For {
        pattern: Pat,
        expression: Expr,
        nodes: Vec<Self>,
    },
    Match {
        expression: Expr,
        arms: Vec<MatchArm>,
    },
    Let {
        pattern: Pat,
        value: Expr,
    },
    Rust(TokenStream2),
}

struct MatchArm {
    pattern: Pat,
    guard: Option<Expr>,
    nodes: Vec<Node>,
}

fn parse_nodes(input: ParseStream<'_>) -> Result<Vec<Node>> {
    let mut nodes = Vec::new();
    while !input.is_empty() {
        nodes.push(parse_node(input)?);
    }
    Ok(nodes)
}

fn parse_node(input: ParseStream<'_>) -> Result<Node> {
    if input.peek(Token![@]) {
        return parse_directive(input);
    }

    let kind: Path = input.parse()?;
    if kind.segments.len() == 1 && kind.segments[0].ident == "Rect" && input.peek(syn::token::Paren)
    {
        let args;
        parenthesized!(args in input);
        let key = parse_property_value(&args)?;
        args.parse::<Token![,]>()?;
        let rect = args.parse()?;
        if args.peek(Token![,]) {
            args.parse::<Token![,]>()?;
        }
        if !args.is_empty() {
            return Err(args.error("Rect expects exactly `key, rect`"));
        }
        let content;
        braced!(content in input);
        return Ok(Node::Rect {
            key,
            rect,
            entries: parse_entries(&content)?,
        });
    }
    let state = if input.peek(syn::token::Paren) {
        let state_content;
        parenthesized!(state_content in input);
        let state = state_content.parse()?;
        if !state_content.is_empty() {
            return Err(state_content.error("component state expects one expression"));
        }
        Some(state)
    } else {
        None
    };
    let content;
    braced!(content in input);
    Ok(Node::Element(Element {
        kind,
        state,
        entries: parse_entries(&content)?,
    }))
}

fn parse_entries(content: ParseStream<'_>) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    while !content.is_empty() {
        let fork = content.fork();
        let is_property = fork.parse::<Ident>().is_ok() && {
            let _ = fork.parse::<Token![!]>();
            fork.peek(Token![:]) || fork.peek(Token![;])
        };
        if is_property {
            let name = content.parse()?;
            let component = content.parse::<Token![!]>().is_ok();
            let value = if content.parse::<Token![:]>().is_ok() {
                Some(parse_property_value(content)?)
            } else {
                None
            };
            content.parse::<Token![;]>()?;
            entries.push(Entry::Property(Property {
                name,
                value,
                component,
            }));
        } else {
            entries.push(Entry::Node(parse_node(content)?));
        }
    }
    Ok(entries)
}
fn parse_property_value(input: ParseStream<'_>) -> Result<Expr> {
    if !input.peek(Token![@]) {
        return input.parse();
    }
    input.parse::<Token![@]>()?;
    let helper = input.call(Ident::parse_any)?;
    if helper != "format" {
        return Err(Error::new(
            helper.span(),
            "unknown property helper; expected `@format(...)`",
        ));
    }
    let args;
    parenthesized!(args in input);
    let args: TokenStream2 = args.parse()?;
    let core = core_path();
    syn::parse2(quote! { #core::FormatKey::new(format_args!(#args)) })
}

fn parse_directive(input: ParseStream<'_>) -> Result<Node> {
    input.parse::<Token![@]>()?;
    let directive = input.call(Ident::parse_any)?;
    match directive.to_string().as_str() {
        "if" => {
            let condition = Expr::parse_without_eager_brace(input)?;
            let content;
            braced!(content in input);
            let then_nodes = parse_nodes(&content)?;
            let else_branch = if input.peek(Token![@]) {
                let fork = input.fork();
                fork.parse::<Token![@]>()?;
                let name = fork.call(Ident::parse_any)?;
                if name == "else" {
                    input.parse::<Token![@]>()?;
                    input.call(Ident::parse_any)?;
                    if input.peek(Token![@]) {
                        input.parse::<Token![@]>()?;
                        let nested = input.call(Ident::parse_any)?;
                        if nested != "if" {
                            return Err(Error::new(nested.span(), "expected `if` after `@else @`"));
                        }
                        let condition = Expr::parse_without_eager_brace(input)?;
                        let body;
                        braced!(body in input);
                        Some(Box::new(Node::If {
                            condition,
                            then_nodes: parse_nodes(&body)?,
                            else_branch: None,
                        }))
                    } else {
                        let body;
                        braced!(body in input);
                        Some(Box::new(Node::Rust(expand_nodes(parse_nodes(&body)?)?)))
                    }
                } else {
                    None
                }
            } else {
                None
            };
            Ok(Node::If {
                condition,
                then_nodes,
                else_branch,
            })
        }
        "for" => {
            let pattern = Pat::parse_multi_with_leading_vert(input)?;
            input.parse::<Token![in]>()?;
            let expression = Expr::parse_without_eager_brace(input)?;
            let content;
            braced!(content in input);
            Ok(Node::For {
                pattern,
                expression,
                nodes: parse_nodes(&content)?,
            })
        }
        "match" => {
            let expression = Expr::parse_without_eager_brace(input)?;
            let content;
            braced!(content in input);
            let mut arms = Vec::new();
            while !content.is_empty() {
                let pattern = Pat::parse_multi_with_leading_vert(&content)?;
                let guard = if content.peek(Token![if]) {
                    content.parse::<Token![if]>()?;
                    Some(content.parse()?)
                } else {
                    None
                };
                content.parse::<Token![=>]>()?;
                let body;
                braced!(body in content);
                arms.push(MatchArm {
                    pattern,
                    guard,
                    nodes: parse_nodes(&body)?,
                });
                if content.peek(Token![,]) {
                    content.parse::<Token![,]>()?;
                }
            }
            Ok(Node::Match { expression, arms })
        }
        "let" => {
            let pattern = Pat::parse_multi_with_leading_vert(input)?;
            input.parse::<Token![=]>()?;
            let value = input.parse()?;
            input.parse::<Token![;]>()?;
            Ok(Node::Let { pattern, value })
        }
        "rust" => {
            let content;
            braced!(content in input);
            Ok(Node::Rust(content.parse()?))
        }
        _ => Err(Error::new(
            directive.span(),
            "unknown UI directive; expected `if`, `for`, `match`, `let`, or `rust`",
        )),
    }
}

fn expand_input(input: UiInput) -> Result<TokenStream2> {
    let ctx = input.ctx;
    let nodes = expand_nodes(input.nodes)?;
    let core = core_path();
    Ok(quote! { (|ctx: &mut #core::BuildCtx| { #nodes })(&mut *(#ctx)) })
}

fn expand_nodes(nodes: Vec<Node>) -> Result<TokenStream2> {
    let expanded = nodes
        .into_iter()
        .map(expand_node)
        .collect::<Result<Vec<_>>>()?;
    Ok(quote! { #(#expanded)* })
}

fn expand_node(node: Node) -> Result<TokenStream2> {
    match node {
        Node::Element(element) => expand_element(element),
        Node::Rect { key, rect, entries } => {
            expand_block(entries, quote! { ctx.rect(#key, #rect) })
        }
        Node::If {
            condition,
            then_nodes,
            else_branch,
        } => {
            let then_nodes = expand_nodes(then_nodes)?;
            let else_branch = else_branch
                .map(|node| expand_node(*node))
                .transpose()?
                .map(|branch| quote! { else { #branch } });
            Ok(quote! { if #condition { #then_nodes } #else_branch })
        }
        Node::For {
            pattern,
            expression,
            nodes,
        } => {
            let nodes = expand_nodes(nodes)?;
            Ok(quote! { for #pattern in #expression { #nodes } })
        }
        Node::Match { expression, arms } => {
            let arms = arms
                .into_iter()
                .map(|arm| {
                    let pattern = arm.pattern;
                    let guard = arm.guard.map(|guard| quote! { if #guard });
                    let nodes = expand_nodes(arm.nodes)?;
                    Ok(quote! { #pattern #guard => { #nodes } })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(quote! { match #expression { #(#arms),* } })
        }
        Node::Let { pattern, value } => Ok(quote! { let #pattern = #value; }),
        Node::Rust(tokens) => Ok(tokens),
    }
}

fn expand_element(element: Element) -> Result<TokenStream2> {
    let name = element
        .kind
        .segments
        .last()
        .expect("non-empty path")
        .ident
        .to_string();
    match name.as_str() {
        "Block" if element.kind.segments.len() == 1 => {
            if let Some(state) = element.state {
                return Err(Error::new_spanned(
                    state,
                    "Block does not accept component state",
                ));
            }
            expand_block(element.entries, quote! { ctx.new() })
        }
        _ => expand_component(element),
    }
}

fn expand_component(element: Element) -> Result<TokenStream2> {
    let core = core_path();
    let alias = element
        .kind
        .segments
        .last()
        .expect("non-empty path")
        .ident
        .clone();
    let marker_ident = format_ident!("{alias}Ui");
    let props_ident = format_ident!("{alias}UiProps");
    let built_in = matches!(
        alias.to_string().as_str(),
        "Row" | "Column" | "HSpacer" | "VSpacer" | "Icon"
    );
    let marker = if built_in {
        quote! { #core::components::#marker_ident }
    } else {
        let mut path = element.kind.clone();
        path.segments.last_mut().expect("non-empty path").ident = marker_ident;
        quote! { #path }
    };
    let props = if built_in {
        quote! { #core::components::#props_ident }
    } else {
        let mut path = element.kind;
        path.segments.last_mut().expect("non-empty path").ident = props_ident;
        quote! { #path }
    };

    let (properties, children) = split_entries(element.entries);
    let mut component_fields = Vec::new();
    let mut block_properties = Vec::new();
    let mut seen = HashSet::new();
    for property in properties {
        let text = property.name.to_string();
        if !seen.insert(text.clone()) {
            return Err(Error::new(
                property.name.span(),
                format!("duplicate component property `{text}`"),
            ));
        }
        if property.component {
            let name = property.name;
            let Some(value) = property.value else {
                return Err(Error::new(name.span(), "component property requires value"));
            };
            component_fields.push(quote! { #name: #value });
        } else {
            block_properties.push(property);
        }
    }

    let props = quote! { #props { #(#component_fields),* } };
    let start = if let Some(state) = element.state {
        quote! { #marker::begin(#state, ctx, #props) }
    } else {
        quote! { #marker::begin(ctx, #props) }
    };
    expand_block_parts(block_properties, children, start)
}

fn split_entries(entries: Vec<Entry>) -> (Vec<Property>, Vec<Node>) {
    let mut properties = Vec::new();
    let mut nodes = Vec::new();
    for entry in entries {
        match entry {
            Entry::Property(property) => properties.push(property),
            Entry::Node(node) => nodes.push(node),
        }
    }
    (properties, nodes)
}

fn expand_block(entries: Vec<Entry>, start: TokenStream2) -> Result<TokenStream2> {
    let (properties, children) = split_entries(entries);
    expand_block_parts(properties, children, start)
}

fn expand_block_parts(
    properties: Vec<Property>,
    children: Vec<Node>,
    mut builder: TokenStream2,
) -> Result<TokenStream2> {
    let mut seen = HashSet::new();
    for property in properties {
        if property.component {
            return Err(Error::new(
                property.name.span(),
                "component property is only valid on component nodes",
            ));
        }
        let name = property.name;
        if !seen.insert(name.to_string()) {
            return Err(Error::new(name.span(), "duplicate UI property"));
        }
        builder = match property.value {
            None => quote! { #builder.#name() },
            Some(value) => quote! { #builder.#name(#value) },
        };
    }

    if children.is_empty() {
        Ok(quote! { { #builder.build(); } })
    } else {
        let children = expand_nodes(children)?;
        Ok(quote! { { #builder.children(|ctx| { #children }).build(); } })
    }
}

fn core_path() -> TokenStream2 {
    match proc_macro_crate::crate_name("kama-ui") {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote! { crate },
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let name = Ident::new(&name, Span::call_site());
            quote! { ::#name }
        }
        Err(_) => quote! { ::kama_ui },
    }
}

struct ComponentArgs {
    alias: LitStr,
    stateless: bool,
}

impl Parse for ComponentArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let alias = input.parse()?;
        let mut stateless = false;
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            let option = input.call(Ident::parse_any)?;
            if option != "stateless" {
                return Err(Error::new(option.span(), "expected `stateless`"));
            }
            stateless = true;
        }
        if !input.is_empty() {
            return Err(input.error("unexpected ui_component options"));
        }
        Ok(Self { alias, stateless })
    }
}

fn expand_component_impl(args: ComponentArgs, item: ItemImpl) -> Result<TokenStream2> {
    if !item.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &item.generics,
            "generic component impls are not supported yet",
        ));
    }
    let Some((trait_path, _)) = &item.trait_ else {
        return Err(Error::new_spanned(
            &item.self_ty,
            "ui_component must annotate `impl Component for Type`",
        ));
    };
    if trait_path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "Component")
    {
        return Err(Error::new_spanned(
            trait_path,
            "ui_component expects `impl Component for Type`",
        ));
    }
    let methods = item
        .items
        .iter()
        .filter_map(|item| match item {
            syn::ImplItem::Fn(method) if method.sig.ident == "ui" => Some(method),
            _ => None,
        })
        .collect::<Vec<_>>();
    if methods.len() != 1 {
        return Err(Error::new_spanned(
            &item,
            "component impl requires exactly one `ui` method",
        ));
    }
    if item.items.len() != 1 {
        return Err(Error::new_spanned(
            &item,
            "component impl may only contain `ui`; put other methods in inherent impl",
        ));
    }
    let method = methods[0];
    if !method.sig.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &method.sig.generics,
            "generic component UI methods are unsupported; return lifetime is generated",
        ));
    }
    let mut inputs = method.sig.inputs.iter();
    let Some(FnArg::Receiver(receiver)) = inputs.next() else {
        return Err(Error::new_spanned(
            &method.sig,
            "ui method requires `&mut self`",
        ));
    };
    if !matches!(receiver.kind, ReceiverKind::Reference(_, _, Some(_))) {
        return Err(Error::new_spanned(
            receiver,
            "ui method requires `&mut self`",
        ));
    }
    let Some(FnArg::Typed(ctx_arg)) = inputs.next() else {
        return Err(Error::new_spanned(
            &method.sig,
            "ui method requires BuildCtx as second argument",
        ));
    };
    let Pat::Ident(ctx_pattern) = &*ctx_arg.pat else {
        return Err(Error::new_spanned(
            &ctx_arg.pat,
            "context argument requires identifier",
        ));
    };
    let ctx_name = &ctx_pattern.ident;

    let mut field_names = Vec::new();
    let mut field_types = Vec::<Type>::new();
    for input in inputs {
        let FnArg::Typed(input) = input else {
            return Err(Error::new_spanned(input, "unexpected receiver"));
        };
        let Pat::Ident(pattern) = &*input.pat else {
            return Err(Error::new_spanned(
                &input.pat,
                "component property requires identifier pattern",
            ));
        };
        if matches!(&*input.ty, Type::ImplTrait(_)) {
            return Err(Error::new_spanned(
                &input.ty,
                "`impl Trait` component properties are unsupported; use concrete owned type",
            ));
        }
        field_names.push(pattern.ident.clone());
        field_types.push((*input.ty).clone());
    }

    let alias = args.alias.value();
    let alias_ident = syn::parse_str::<Ident>(&alias)
        .map_err(|_| Error::new(args.alias.span(), "component name must be Rust identifier"))?;
    let marker = format_ident!("{alias_ident}Ui");
    let props = format_ident!("{alias_ident}UiProps");
    let state = &item.self_ty;
    let body = &method.block.stmts;
    let core = core_path();
    let begin = if args.stateless {
        quote! {
            pub fn begin<'ui>(
                #ctx_name: &'ui mut #core::BuildCtx,
                props: #props,
            ) -> #core::BlockBuilder<'ui> {
                let mut component: #state = ::core::default::Default::default();
                #core::Component::ui(&mut component, #ctx_name, props)
            }
        }
    } else {
        quote! {
            pub fn begin<'ui>(
                component: &mut #state,
                #ctx_name: &'ui mut #core::BuildCtx,
                props: #props,
            ) -> #core::BlockBuilder<'ui> {
                #core::Component::ui(component, #ctx_name, props)
            }
        }
    };

    Ok(quote! {
        pub struct #props {
            #(pub #field_names: #field_types),*
        }

        pub struct #marker;
        impl #marker { #begin }

        impl #core::Component<#props> for #state {
            fn ui<'ui>(
                &mut self,
                #ctx_name: &'ui mut #core::BuildCtx,
                props: #props,
            ) -> #core::BlockBuilder<'ui> {
                let #props { #(#field_names),* } = props;
                #(#body)*
            }
        }
    })
}
