//! Code generation for `#[derive(GraphQLScalar)]` macro.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote, ToTokens, TokenStreamExt};
use syn::{
    ext::IdentExt as _,
    parse::{Parse, ParseStream},
    parse_quote,
    spanned::Spanned,
    token,
};
use url::Url;

use crate::{
    common::{
        parse::{
            attr::{err, OptionExt},
            ParseBufferExt as _,
        },
        scalar,
    },
    result::GraphQLScope,
    util::{filter_attrs, get_doc_comment, span_container::SpanContainer},
};

/// [`GraphQLScope`] of errors for `#[derive(GraphQLScalar)]` macro.
const ERR: GraphQLScope = GraphQLScope::DeriveScalar;

/// Expands `#[derive(GraphQLScalar)]` macro into generated code.
pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let ast = syn::parse2::<syn::DeriveInput>(input)?;

    let attr = Attr::from_attrs("graphql", &ast.attrs)?;

    let field = match (
        attr.to_output.as_deref().cloned(),
        attr.from_input.as_deref().cloned(),
        attr.from_input_err.as_deref().cloned(),
        attr.parse_token.as_deref().cloned(),
        attr.with.as_deref().cloned(),
    ) {
        (Some(to_output), Some(from_input), Some(from_input_err), Some(parse_token), None) => {
            GraphQLScalarMethods::Custom {
                to_output,
                from_input: (from_input, from_input_err),
                parse_token,
            }
        }
        (to_output, from_input, from_input_err, parse_token, Some(module)) => {
            GraphQLScalarMethods::Custom {
                to_output: to_output.unwrap_or_else(|| parse_quote! { #module::to_output }),
                from_input: (
                    from_input.unwrap_or_else(|| parse_quote! { #module::from_input }),
                    from_input_err.unwrap_or_else(|| parse_quote! { #module::Error }),
                ),
                parse_token: parse_token
                    .unwrap_or_else(|| ParseToken::Custom(parse_quote! { #module::parse_token })),
            }
        }
        (to_output, from_input, from_input_err, parse_token, None) => {
            let from_input = match (from_input, from_input_err) {
                (Some(from_input), Some(err)) => Some((from_input, err)),
                (None, None) => None,
                _ => {
                    return Err(ERR.custom_error(
                        ast.span(),
                        "`from_input_with` attribute should be provided in \
                         tandem with `from_input_err`",
                    ))
                }
            };

            let data = if let syn::Data::Struct(data) = &ast.data {
                data
            } else {
                return Err(ERR.custom_error(
                    ast.span(),
                    "expected all custom resolvers or single-field struct",
                ));
            };
            let field = match &data.fields {
                syn::Fields::Unit => Err(ERR.custom_error(
                    ast.span(),
                    "expected exactly 1 field, e.g.: `Test(i32)`, `Test { test: i32 }` \
                     or all custom resolvers",
                )),
                syn::Fields::Unnamed(fields) => fields
                    .unnamed
                    .first()
                    .and_then(|f| (fields.unnamed.len() == 1).then(|| Field::Unnamed(f.clone())))
                    .ok_or_else(|| {
                        ERR.custom_error(
                            ast.span(),
                            "expected exactly 1 field, e.g., Test(i32) \
                             or all custom resolvers",
                        )
                    }),
                syn::Fields::Named(fields) => fields
                    .named
                    .first()
                    .and_then(|f| (fields.named.len() == 1).then(|| Field::Named(f.clone())))
                    .ok_or_else(|| {
                        ERR.custom_error(
                            ast.span(),
                            "expected exactly 1 field, e.g., Test { test: i32 } \
                             or all custom resolvers",
                        )
                    }),
            }?;
            GraphQLScalarMethods::Delegated {
                to_output,
                from_input,
                parse_token,
                field,
            }
        }
    };

    let scalar = scalar::Type::parse(attr.scalar.as_deref(), &ast.generics);

    Ok(Definition {
        ident: ast.ident.clone(),
        generics: ast.generics.clone(),
        methods: field,
        name: attr
            .name
            .as_deref()
            .cloned()
            .unwrap_or_else(|| ast.ident.to_string()),
        description: attr.description.as_deref().cloned(),
        specified_by_url: attr.specified_by_url.as_deref().cloned(),
        scalar,
    }
    .to_token_stream())
}

/// Available arguments behind `#[graphql]` attribute when generating
/// code for `#[derive(GraphQLScalar)]`.
#[derive(Default)]
struct Attr {
    /// Name of this [GraphQL scalar][1] in GraphQL schema.
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    name: Option<SpanContainer<String>>,

    /// Description of this [GraphQL scalar][1] to put into GraphQL schema.
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    description: Option<SpanContainer<String>>,

    /// Spec [`Url`] of this [GraphQL scalar][1] to put into GraphQL schema.
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    specified_by_url: Option<SpanContainer<Url>>,

    /// Explicitly specified type (or type parameter with its bounds) of
    /// [`ScalarValue`] to use for resolving this [GraphQL scalar][1] type with.
    ///
    /// If [`None`], then generated code will be generic over any
    /// [`ScalarValue`] type, which, in turn, requires all [scalar][1] fields to
    /// be generic over any [`ScalarValue`] type too. That's why this type
    /// should be specified only if one of the variants implements
    /// [`GraphQLType`] in a non-generic way over [`ScalarValue`] type.
    ///
    /// [`GraphQLType`]: juniper::GraphQLType
    /// [`ScalarValue`]: juniper::ScalarValue
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    scalar: Option<SpanContainer<scalar::AttrValue>>,

    /// Explicitly specified function to be used instead of
    /// [`GraphQLScalar::to_output`].
    ///
    /// [`GraphQLScalar::to_output`]: juniper::GraphQLScalar::to_output
    to_output: Option<SpanContainer<syn::ExprPath>>,

    /// Explicitly specified function to be used instead of
    /// [`GraphQLScalar::from_input`].
    ///
    /// [`GraphQLScalar::from_input`]: juniper::GraphQLScalar::from_input
    from_input: Option<SpanContainer<syn::ExprPath>>,

    /// Explicitly specified type to be used instead of
    /// [`GraphQLScalar::Error`].
    ///
    /// [`GraphQLScalar::Error`]: juniper::GraphQLScalar::Error
    from_input_err: Option<SpanContainer<syn::Type>>,

    /// Explicitly specified resolver to be used instead of
    /// [`GraphQLScalar::parse_token`].
    ///
    /// [`GraphQLScalar::parse_token`]: juniper::GraphQLScalar::parse_token
    parse_token: Option<SpanContainer<ParseToken>>,

    /// Explicitly specified module with all custom resolvers for
    /// [`Self::to_output`], [`Self::from_input`], [`Self::from_input_err`] and
    /// [`Self::parse_token`].
    with: Option<SpanContainer<syn::ExprPath>>,
}

impl Parse for Attr {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut out = Self::default();
        while !input.is_empty() {
            let ident = input.parse_any_ident()?;
            match ident.to_string().as_str() {
                "name" => {
                    input.parse::<token::Eq>()?;
                    let name = input.parse::<syn::LitStr>()?;
                    out.name
                        .replace(SpanContainer::new(
                            ident.span(),
                            Some(name.span()),
                            name.value(),
                        ))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "desc" | "description" => {
                    input.parse::<token::Eq>()?;
                    let desc = input.parse::<syn::LitStr>()?;
                    out.description
                        .replace(SpanContainer::new(
                            ident.span(),
                            Some(desc.span()),
                            desc.value(),
                        ))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "specified_by_url" => {
                    input.parse::<token::Eq>()?;
                    let lit = input.parse::<syn::LitStr>()?;
                    let url = lit.value().parse::<Url>().map_err(|err| {
                        syn::Error::new(lit.span(), format!("Invalid URL: {}", err))
                    })?;
                    out.specified_by_url
                        .replace(SpanContainer::new(ident.span(), Some(lit.span()), url))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "scalar" | "Scalar" | "ScalarValue" => {
                    input.parse::<token::Eq>()?;
                    let scl = input.parse::<scalar::AttrValue>()?;
                    out.scalar
                        .replace(SpanContainer::new(ident.span(), Some(scl.span()), scl))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "to_output_with" => {
                    input.parse::<token::Eq>()?;
                    let scl = input.parse::<syn::ExprPath>()?;
                    out.to_output
                        .replace(SpanContainer::new(ident.span(), Some(scl.span()), scl))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "from_input_with" => {
                    input.parse::<token::Eq>()?;
                    let scl = input.parse::<syn::ExprPath>()?;
                    out.from_input
                        .replace(SpanContainer::new(ident.span(), Some(scl.span()), scl))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "from_input_err" => {
                    input.parse::<token::Eq>()?;
                    let scl = input.parse::<syn::Type>()?;
                    out.from_input_err
                        .replace(SpanContainer::new(ident.span(), Some(scl.span()), scl))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "parse_token_with" => {
                    input.parse::<token::Eq>()?;
                    let scl = input.parse::<syn::ExprPath>()?;
                    out.parse_token
                        .replace(SpanContainer::new(
                            ident.span(),
                            Some(scl.span()),
                            ParseToken::Custom(scl),
                        ))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "parse_token" => {
                    let (span, parsed_types) = if input.parse::<token::Eq>().is_ok() {
                        let scl = input.parse::<syn::Type>()?;
                        (scl.span(), vec![scl])
                    } else {
                        let types;
                        let _ = syn::parenthesized!(types in input);
                        let parsed_types =
                            types.parse_terminated::<_, token::Comma>(syn::Type::parse)?;

                        if parsed_types.is_empty() {
                            return Err(syn::Error::new(ident.span(), "expected at least 1 type."));
                        }

                        (parsed_types.span(), parsed_types.into_iter().collect())
                    };

                    out.parse_token
                        .replace(SpanContainer::new(
                            ident.span(),
                            Some(span),
                            ParseToken::Delegated(parsed_types),
                        ))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                "with" => {
                    input.parse::<token::Eq>()?;
                    let scl = input.parse::<syn::ExprPath>()?;
                    out.with
                        .replace(SpanContainer::new(ident.span(), Some(scl.span()), scl))
                        .none_or_else(|_| err::dup_arg(&ident))?
                }
                name => {
                    return Err(err::unknown_arg(&ident, name));
                }
            }
            input.try_parse::<token::Comma>()?;
        }
        Ok(out)
    }
}

impl Attr {
    /// Tries to merge two [`Attr`]s into a single one, reporting about
    /// duplicates, if any.
    fn try_merge(self, mut another: Self) -> syn::Result<Self> {
        Ok(Self {
            name: try_merge_opt!(name: self, another),
            description: try_merge_opt!(description: self, another),
            specified_by_url: try_merge_opt!(specified_by_url: self, another),
            scalar: try_merge_opt!(scalar: self, another),
            to_output: try_merge_opt!(to_output: self, another),
            from_input: try_merge_opt!(from_input: self, another),
            from_input_err: try_merge_opt!(from_input_err: self, another),
            parse_token: try_merge_opt!(parse_token: self, another),
            with: try_merge_opt!(with: self, another),
        })
    }

    /// Parses [`Attr`] from the given multiple `name`d [`syn::Attribute`]s
    /// placed on a trait definition.
    fn from_attrs(name: &str, attrs: &[syn::Attribute]) -> syn::Result<Self> {
        let mut attr = filter_attrs(name, attrs)
            .map(|attr| attr.parse_args())
            .try_fold(Self::default(), |prev, curr| prev.try_merge(curr?))?;

        if attr.description.is_none() {
            attr.description = get_doc_comment(attrs);
        }

        Ok(attr)
    }
}

/// Definition of [GraphQL scalar][1] for code generation.
///
/// [1]: https://spec.graphql.org/October2021/#sec-Scalars
struct Definition {
    /// Name of this [GraphQL scalar][1] in GraphQL schema.
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    name: String,

    /// Rust type [`Ident`] that this [GraphQL scalar][1] is represented with.
    ///
    /// [`Ident`]: syn::Ident
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    ident: syn::Ident,

    /// Generics of the Rust type that this [GraphQL scalar][1] is implemented
    /// for.
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    generics: syn::Generics,

    /// [`GraphQLScalarDefinition`] representing [GraphQL scalar][1].
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    methods: GraphQLScalarMethods,

    /// Description of this [GraphQL scalar][1] to put into GraphQL schema.
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    description: Option<String>,

    /// Spec [`Url`] of this [GraphQL scalar][1] to put into GraphQL schema.
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    specified_by_url: Option<Url>,

    /// [`ScalarValue`] parametrization to generate [`GraphQLType`]
    /// implementation with for this [GraphQL scalar][1].
    ///
    /// [`GraphQLType`]: juniper::GraphQLType
    /// [`ScalarValue`]: juniper::ScalarValue
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    scalar: scalar::Type,
}

impl ToTokens for Definition {
    fn to_tokens(&self, into: &mut TokenStream) {
        self.impl_output_and_input_type_tokens().to_tokens(into);
        self.impl_type_tokens().to_tokens(into);
        self.impl_value_tokens().to_tokens(into);
        self.impl_value_async_tokens().to_tokens(into);
        self.impl_to_input_value_tokens().to_tokens(into);
        self.impl_from_input_value_tokens().to_tokens(into);
        self.impl_parse_scalar_value_tokens().to_tokens(into);
        self.impl_graphql_scalar_tokens().to_tokens(into);
        self.impl_reflection_traits_tokens().to_tokens(into);
    }
}

impl Definition {
    /// Returns generated code implementing [`marker::IsInputType`] and
    /// [`marker::IsOutputType`] trait for this [GraphQL scalar][1].
    ///
    /// [`marker::IsInputType`]: juniper::marker::IsInputType
    /// [`marker::IsOutputType`]: juniper::marker::IsOutputType
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    #[must_use]
    fn impl_output_and_input_type_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;

        let generics = self.impl_generics(false);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::marker::IsInputType<#scalar> for #ident#ty_gens
                #where_clause { }

            #[automatically_derived]
            impl#impl_gens ::juniper::marker::IsOutputType<#scalar> for #ident#ty_gens
                #where_clause { }
        }
    }

    /// Returns generated code implementing [`GraphQLType`] trait for this
    /// [GraphQL scalar][1].
    ///
    /// [`GraphQLType`]: juniper::GraphQLType
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn impl_type_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;
        let name = &self.name;

        let description = self
            .description
            .as_ref()
            .map(|val| quote! { .description(#val) });
        let specified_by_url = self.specified_by_url.as_ref().map(|url| {
            let url_lit = url.as_str();
            quote! { .specified_by_url(#url_lit) }
        });

        let generics = self.impl_generics(false);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::GraphQLType<#scalar> for #ident#ty_gens
                #where_clause
            {
                fn name(_: &Self::TypeInfo) -> Option<&'static str> {
                    Some(#name)
                }

                fn meta<'r>(
                    info: &Self::TypeInfo,
                    registry: &mut ::juniper::Registry<'r, #scalar>,
                ) -> ::juniper::meta::MetaType<'r, #scalar>
                where
                    #scalar: 'r,
                {
                    registry.build_scalar_type::<Self>(info)
                        #description
                        #specified_by_url
                        .into_meta()
                }
            }
        }
    }

    /// Returns generated code implementing [`GraphQLValue`] trait for this
    /// [GraphQL scalar][1].
    ///
    /// [`GraphQLValue`]: juniper::GraphQLValue
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn impl_value_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;

        let resolve = self.methods.expand_resolve(scalar);

        let generics = self.impl_generics(false);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::GraphQLValue<#scalar> for #ident#ty_gens
                #where_clause
            {
                type Context = ();
                type TypeInfo = ();

                fn type_name<'i>(&self, info: &'i Self::TypeInfo) -> Option<&'i str> {
                    <Self as ::juniper::GraphQLType<#scalar>>::name(info)
                }

                fn resolve(
                    &self,
                    info: &(),
                    selection: Option<&[::juniper::Selection<#scalar>]>,
                    executor: &::juniper::Executor<Self::Context, #scalar>,
                ) -> ::juniper::ExecutionResult<#scalar> {
                    #resolve
                }
            }
        }
    }

    /// Returns generated code implementing [`GraphQLValueAsync`] trait for this
    /// [GraphQL scalar][1].
    ///
    /// [`GraphQLValueAsync`]: juniper::GraphQLValueAsync
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn impl_value_async_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;

        let generics = self.impl_generics(true);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::GraphQLValueAsync<#scalar> for #ident#ty_gens
                #where_clause
            {
                fn resolve_async<'b>(
                    &'b self,
                    info: &'b Self::TypeInfo,
                    selection_set: Option<&'b [::juniper::Selection<#scalar>]>,
                    executor: &'b ::juniper::Executor<Self::Context, #scalar>,
                ) -> ::juniper::BoxFuture<'b, ::juniper::ExecutionResult<#scalar>> {
                    use ::juniper::futures::future;
                    let v = ::juniper::GraphQLValue::resolve(self, info, selection_set, executor);
                    Box::pin(future::ready(v))
                }
            }
        }
    }

    /// Returns generated code implementing [`InputValue`] trait for this
    /// [GraphQL scalar][1].
    ///
    /// [`InputValue`]: juniper::InputValue
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn impl_to_input_value_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;

        let to_input_value = self.methods.expand_to_input_value(scalar);

        let generics = self.impl_generics(false);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::ToInputValue<#scalar> for #ident#ty_gens
                #where_clause
            {
                fn to_input_value(&self) -> ::juniper::InputValue<#scalar> {
                    #to_input_value
                }
            }
        }
    }

    /// Returns generated code implementing [`FromInputValue`] trait for this
    /// [GraphQL scalar][1].
    ///
    /// [`FromInputValue`]: juniper::FromInputValue
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn impl_from_input_value_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;

        let error_ty = self.methods.expand_from_input_err(scalar);
        let from_input_value = self.methods.expand_from_input(scalar);

        let generics = self.impl_generics(false);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::FromInputValue<#scalar> for #ident#ty_gens
                #where_clause
            {
                type Error = #error_ty;

                fn from_input_value(input: &::juniper::InputValue<#scalar>) -> Result<Self, Self::Error> {
                   #from_input_value
                }
            }
        }
    }

    /// Returns generated code implementing [`ParseScalarValue`] trait for this
    /// [GraphQL scalar][1].
    ///
    /// [`ParseScalarValue`]: juniper::ParseScalarValue
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn impl_parse_scalar_value_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;

        let from_str = self.methods.expand_parse_token(scalar);

        let generics = self.impl_generics(false);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::ParseScalarValue<#scalar> for #ident#ty_gens
                #where_clause
           {
               fn from_str(
                    token: ::juniper::parser::ScalarToken<'_>,
               ) -> ::juniper::ParseScalarResult<'_, #scalar> {
                    #from_str
                }
            }
        }
    }

    /// Returns generated code implementing [`GraphQLScalar`] trait for this
    /// [GraphQL scalar][1].
    ///
    /// [`GraphQLScalar`]: juniper::GraphQLScalar
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn impl_graphql_scalar_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;

        let generics = self.impl_generics(false);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        let to_output = self.methods.expand_to_output(scalar);
        let from_input_err = self.methods.expand_from_input_err(scalar);
        let from_input = self.methods.expand_from_input(scalar);
        let parse_token = self.methods.expand_parse_token(scalar);

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::GraphQLScalar<#scalar> for #ident#ty_gens
                #where_clause
            {
                type Error = #from_input_err;

                fn to_output(&self) -> ::juniper::Value<#scalar> {
                    #to_output
                }

                fn from_input(
                    input: &::juniper::InputValue<#scalar>
                ) -> Result<Self, Self::Error> {
                    #from_input
                }

                fn parse_token(
                    token: ::juniper::ScalarToken<'_>
                ) -> ::juniper::ParseScalarResult<'_, #scalar> {
                    #parse_token
                }
            }
        }
    }

    /// Returns generated code implementing [`BaseType`], [`BaseSubTypes`] and
    /// [`WrappedType`] traits for this [GraphQL scalar][1].
    ///
    /// [`BaseSubTypes`]: juniper::macros::reflection::BaseSubTypes
    /// [`BaseType`]: juniper::macros::reflection::BaseType
    /// [`WrappedType`]: juniper::macros::reflection::WrappedType
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn impl_reflection_traits_tokens(&self) -> TokenStream {
        let ident = &self.ident;
        let scalar = &self.scalar;
        let name = &self.name;

        let generics = self.impl_generics(false);
        let (impl_gens, _, where_clause) = generics.split_for_impl();
        let (_, ty_gens, _) = self.generics.split_for_impl();

        quote! {
            #[automatically_derived]
            impl#impl_gens ::juniper::macros::reflect::BaseType<#scalar> for #ident#ty_gens
                #where_clause
            {
                const NAME: ::juniper::macros::reflect::Type = #name;
            }

            #[automatically_derived]
            impl#impl_gens ::juniper::macros::reflect::BaseSubTypes<#scalar> for #ident#ty_gens
                #where_clause
            {
                const NAMES: ::juniper::macros::reflect::Types =
                    &[<Self as ::juniper::macros::reflect::BaseType<#scalar>>::NAME];
            }

            #[automatically_derived]
            impl#impl_gens ::juniper::macros::reflect::WrappedType<#scalar> for #ident#ty_gens
                #where_clause
            {
                const VALUE: ::juniper::macros::reflect::WrappedValue = 1;
            }
        }
    }

    /// Returns prepared [`syn::Generics`] for [`GraphQLType`] trait (and
    /// similar) implementation of this enum.
    ///
    /// If `for_async` is `true`, then additional predicates are added to suit
    /// the [`GraphQLAsyncValue`] trait (and similar) requirements.
    ///
    /// [`GraphQLAsyncValue`]: juniper::GraphQLAsyncValue
    /// [`GraphQLType`]: juniper::GraphQLType
    #[must_use]
    fn impl_generics(&self, for_async: bool) -> syn::Generics {
        let mut generics = self.generics.clone();

        let scalar = &self.scalar;
        if scalar.is_implicit_generic() {
            generics.params.push(parse_quote! { #scalar });
        }
        if scalar.is_generic() {
            generics
                .make_where_clause()
                .predicates
                .push(parse_quote! { #scalar: ::juniper::ScalarValue });
        }
        if let Some(bound) = scalar.bounds() {
            generics.make_where_clause().predicates.push(bound);
        }

        if for_async {
            let self_ty = if self.generics.lifetimes().next().is_some() {
                // Modify lifetime names to omit "lifetime name `'a` shadows a
                // lifetime name that is already in scope" error.
                let mut generics = self.generics.clone();
                for lt in generics.lifetimes_mut() {
                    let ident = lt.lifetime.ident.unraw();
                    lt.lifetime.ident = format_ident!("__fa__{}", ident);
                }

                let lifetimes = generics.lifetimes().map(|lt| &lt.lifetime);
                let ty = &self.ident;
                let (_, ty_generics, _) = generics.split_for_impl();

                quote! { for<#( #lifetimes ),*> #ty#ty_generics }
            } else {
                quote! { Self }
            };
            generics
                .make_where_clause()
                .predicates
                .push(parse_quote! { #self_ty: Sync });

            if scalar.is_generic() {
                generics
                    .make_where_clause()
                    .predicates
                    .push(parse_quote! { #scalar: Send + Sync });
            }
        }

        generics
    }
}

/// Methods representing [GraphQL scalar][1].
///
/// [1]: https://spec.graphql.org/October2021/#sec-Scalars
enum GraphQLScalarMethods {
    /// [GraphQL scalar][1] represented with only custom resolvers.
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    Custom {
        /// Function provided with `#[graphql(to_output_with = ...)]`.
        to_output: syn::ExprPath,

        /// Function and return type provided with
        /// `#[graphql(from_input_with = ..., from_input_err = ...)]`.
        from_input: (syn::ExprPath, syn::Type),

        /// [`ParseToken`] provided with `#[graphql(parse_token_with = ...)]`
        /// or `#[graphql(parse_token(...))]`.
        parse_token: ParseToken,
    },

    /// [GraphQL scalar][1] maybe partially represented with custom resolver.
    /// Other methods are used from [`Field`].
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    Delegated {
        /// Function provided with `#[graphql(to_output_with = ...)]`.
        to_output: Option<syn::ExprPath>,

        /// Function and return type provided with
        /// `#[graphql(from_input_with = ..., from_input_err = ...)]`.
        from_input: Option<(syn::ExprPath, syn::Type)>,

        /// [`ParseToken`] provided with `#[graphql(parse_token_with = ...)]`
        /// or `#[graphql(parse_token(...))]`.
        parse_token: Option<ParseToken>,

        /// [`Field`] to resolve not provided methods.
        field: Field,
    },
}

impl GraphQLScalarMethods {
    /// Expands [`GraphQLValue::resolve`] method.
    ///
    /// [`GraphQLValue::resolve`]: juniper::GraphQLValue::resolve
    fn expand_resolve(&self, scalar: &scalar::Type) -> TokenStream {
        match self {
            Self::Custom { to_output, .. }
            | Self::Delegated {
                to_output: Some(to_output),
                ..
            } => {
                quote! { Ok(#to_output(self)) }
            }
            Self::Delegated { field, .. } => {
                quote! {
                    ::juniper::GraphQLValue::<#scalar>::resolve(
                        &self.#field,
                        info,
                        selection,
                        executor,
                    )
                }
            }
        }
    }

    /// Expands [`GraphQLScalar::to_output`] method.
    ///
    /// [`GraphQLScalar::to_output`]: juniper::GraphQLScalar::to_output
    fn expand_to_output(&self, scalar: &scalar::Type) -> TokenStream {
        match self {
            Self::Custom { to_output, .. }
            | Self::Delegated {
                to_output: Some(to_output),
                ..
            } => {
                quote! { #to_output(self) }
            }
            Self::Delegated { field, .. } => {
                quote! {
                    ::juniper::GraphQLScalar::<#scalar>::to_output(&self.#field)
                }
            }
        }
    }

    /// Expands [`ToInputValue::to_input_value`] method.
    ///
    /// [`ToInputValue::to_input_value`]: juniper::ToInputValue::to_input_value
    fn expand_to_input_value(&self, scalar: &scalar::Type) -> TokenStream {
        match self {
            Self::Custom { to_output, .. }
            | Self::Delegated {
                to_output: Some(to_output),
                ..
            } => {
                quote! {
                    let v = #to_output(self);
                    ::juniper::ToInputValue::to_input_value(&v)
                }
            }
            Self::Delegated { field, .. } => {
                quote! { ::juniper::ToInputValue::<#scalar>::to_input_value(&self.#field) }
            }
        }
    }

    /// Expands [`FromInputValue::Error`] type.
    ///
    /// [`FromInputValue::Error`]: juniper::FromInputValue::Error
    fn expand_from_input_err(&self, scalar: &scalar::Type) -> TokenStream {
        match self {
            Self::Custom {
                from_input: (_, err),
                ..
            }
            | Self::Delegated {
                from_input: Some((_, err)),
                ..
            } => quote! { #err },
            Self::Delegated { field, .. } => {
                let field_ty = field.ty();
                quote! { <#field_ty as ::juniper::FromInputValue<#scalar>>::Error }
            }
        }
    }

    /// Expands [`FromInputValue::from_input_value`][1] method.
    ///
    /// [1]: juniper::FromInputValue::from_input_value
    fn expand_from_input(&self, scalar: &scalar::Type) -> TokenStream {
        match self {
            Self::Custom {
                from_input: (from_input, _),
                ..
            }
            | Self::Delegated {
                from_input: Some((from_input, _)),
                ..
            } => {
                quote! { #from_input(input) }
            }
            Self::Delegated { field, .. } => {
                let field_ty = field.ty();
                let self_constructor = field.closure_constructor();
                quote! {
                    <#field_ty as ::juniper::FromInputValue<#scalar>>::from_input_value(input)
                        .map(#self_constructor)
                }
            }
        }
    }

    /// Expands [`ParseScalarValue::from_str`] method.
    ///
    /// [`ParseScalarValue::from_str`]: juniper::ParseScalarValue::from_str
    fn expand_parse_token(&self, scalar: &scalar::Type) -> TokenStream {
        match self {
            Self::Custom { parse_token, .. }
            | Self::Delegated {
                parse_token: Some(parse_token),
                ..
            } => {
                let parse_token = parse_token.expand_from_str(scalar);
                quote! { #parse_token }
            }
            Self::Delegated { field, .. } => {
                let field_ty = field.ty();
                quote! { <#field_ty as ::juniper::ParseScalarValue<#scalar>>::from_str(token) }
            }
        }
    }
}

/// Representation of [`ParseScalarValue::from_str`] method.
///
/// [`ParseScalarValue::from_str`]: juniper::ParseScalarValue::from_str
#[derive(Clone)]
enum ParseToken {
    /// Custom method.
    Custom(syn::ExprPath),

    /// Tries to parse using [`syn::Type`]s [`ParseScalarValue`] impls until
    /// first success.
    ///
    /// [`ParseScalarValue`]: juniper::ParseScalarValue
    Delegated(Vec<syn::Type>),
}

impl ParseToken {
    /// Expands [`ParseScalarValue::from_str`] method.
    ///
    /// [`ParseScalarValue::from_str`]: juniper::ParseScalarValue::from_str
    fn expand_from_str(&self, scalar: &scalar::Type) -> TokenStream {
        match self {
            ParseToken::Custom(parse_token) => {
                quote! { #parse_token(token) }
            }
            ParseToken::Delegated(delegated) => delegated
                .iter()
                .fold(None, |acc, ty| {
                    acc.map_or_else(
                        || Some(quote! { <#ty as ::juniper::ParseScalarValue<#scalar>>::from_str(token) }),
                        |prev| {
                            Some(quote! {
                                #prev.or_else(|_| {
                                    <#ty as ::juniper::ParseScalarValue<#scalar>>::from_str(token)
                                })
                            })
                        }
                    )
                })
                .unwrap_or_default(),
        }
    }
}

/// Struct field to resolve not provided methods.
enum Field {
    /// Named [`Field`].
    Named(syn::Field),

    /// Unnamed [`Field`].
    Unnamed(syn::Field),
}

impl ToTokens for Field {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            Field::Named(f) => f.ident.to_tokens(tokens),
            Field::Unnamed(_) => tokens.append(Literal::u8_unsuffixed(0)),
        }
    }
}

impl Field {
    /// [`syn::Type`] of this [`Field`].
    fn ty(&self) -> &syn::Type {
        match self {
            Field::Named(f) | Field::Unnamed(f) => &f.ty,
        }
    }

    /// Closure to construct [GraphQL scalar][1] struct from [`Field`].
    ///
    /// [1]: https://spec.graphql.org/October2021/#sec-Scalars
    fn closure_constructor(&self) -> TokenStream {
        match self {
            Field::Named(syn::Field { ident, .. }) => {
                quote! { |v| Self { #ident: v } }
            }
            Field::Unnamed(_) => quote! { Self },
        }
    }
}