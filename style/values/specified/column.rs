/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Specified types for the column properties.

use crate::derives::*;
use crate::parser::{Parse, ParserContext};
use crate::values::generics::column::{
    GapRepeat, GapRepeatCount, GapRuleItem, GapRuleList as GenericGapRuleList, GenericColumnCount,
    GenericRuleInset,
};
use crate::values::specified::PositiveInteger;
use crate::values::specified::{BorderSideWidth, BorderStyle, Color, LengthPercentage};
use cssparser::Parser;
use style_traits::{ParseError, StyleParseErrorKind};

/// A specified type for `column-count` values.
pub type ColumnCount = GenericColumnCount<PositiveInteger>;

/// A specified gap decoration list.
pub type GapRuleList<T> = GenericGapRuleList<T, PositiveInteger>;

/// A specified value for the `column-rule-color` and `row-rule-color`
/// properties.
pub type LineColorList = GapRuleList<Color>;

/// A specified value for the `column-rule-style` and `row-rule-style`
/// properties.
pub type LineStyleList = GapRuleList<BorderStyle>;

/// A specified value for the `column-rule-width` and `row-rule-width`
/// properties.
pub type LineWidthList = GapRuleList<BorderSideWidth>;

/// A specified value for the rule inset properties.
pub type RuleInset = GenericRuleInset<LengthPercentage>;

impl<T: Parse> Parse for GapRuleList<T> {
    fn parse(context: &ParserContext, input: &mut Parser) -> Result<Self, ParseError> {
        let mut seen_auto = false;
        let items = input.parse_comma_separated(|input| {
            let Ok(repeat) = input.try_parse(|input| {
                input.expect_function_matching("repeat")?;
                input.parse_nested_block(|input| {
                    let count = if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
                        GapRepeatCount::Auto
                    } else {
                        GapRepeatCount::Integer(PositiveInteger::parse(context, input)?)
                    };
                    input.expect_comma()?;
                    let values = input.parse_comma_separated(|i| T::parse(context, i))?;
                    Ok(GapRepeat {
                        count,
                        values: values.into(),
                    })
                })
            }) else {
                return Ok(GapRuleItem::Value(T::parse(context, input)?));
            };
            if matches!(repeat.count, GapRepeatCount::Auto) {
                if seen_auto {
                    return Err(ParseError::custom(StyleParseErrorKind::UnspecifiedError));
                }
                seen_auto = true;
            }
            Ok(GapRuleItem::Repeat(repeat))
        })?;
        Ok(Self(items.into()))
    }
}

/// https://drafts.csswg.org/css-gaps-1/#break
#[allow(missing_docs)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    FromPrimitive,
    Hash,
    MallocSizeOf,
    Parse,
    PartialEq,
    Serialize,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
    ToTyped,
)]
#[repr(u8)]
pub enum RuleBreak {
    None,
    Normal,
    Intersection,
}

/// https://drafts.csswg.org/css-gaps-1/#visibility-items
#[allow(missing_docs)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    FromPrimitive,
    Hash,
    MallocSizeOf,
    Parse,
    PartialEq,
    Serialize,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
    ToTyped,
)]
#[repr(u8)]
pub enum RuleVisibilityItems {
    All,
    Around,
    Between,
    Normal,
}

/// https://drafts.csswg.org/css-gaps-1/#propdef-rule-overlap
#[allow(missing_docs)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    FromPrimitive,
    Hash,
    MallocSizeOf,
    Parse,
    PartialEq,
    Serialize,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
    ToTyped,
)]
#[repr(u8)]
pub enum RuleOverlap {
    RowOverColumn,
    ColumnOverRow,
}

/// https://drafts.csswg.org/css-multicol/#propdef-column-fill
#[allow(missing_docs)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    FromPrimitive,
    Hash,
    MallocSizeOf,
    Parse,
    PartialEq,
    Serialize,
    SpecifiedValueInfo,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
    ToTyped,
)]
#[repr(u8)]
pub enum ColumnFill {
    Balance,
    Auto,
}

/// https://drafts.csswg.org/css-multicol/#propdef-column-span
#[allow(missing_docs)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    FromPrimitive,
    Hash,
    MallocSizeOf,
    Parse,
    PartialEq,
    Serialize,
    SpecifiedValueInfo,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
    ToTyped,
)]
#[repr(u8)]
pub enum ColumnSpan {
    None,
    All,
}
