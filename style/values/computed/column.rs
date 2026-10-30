/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Computed types for the column properties.

use crate::values::computed::PositiveInteger;
use crate::values::computed::{BorderSideWidth, BorderStyle, Color, LengthPercentage};
use crate::values::generics::column::GenericRuleInset;
use crate::values::generics::column::{GapRuleList as GenericGapRuleList, GenericColumnCount};
pub use crate::values::specified::column::{
    ColumnFill, ColumnSpan, RuleBreak, RuleOverlap, RuleVisibilityItems,
};

/// A computed type for `column-count` values.
pub type ColumnCount = GenericColumnCount<PositiveInteger>;

/// A computed gap decoration list.
pub type GapRuleList<T> = GenericGapRuleList<T, PositiveInteger>;

/// A computed value for the `column-rule-color` and `row-rule-color`
/// properties.
pub type LineColorList = GapRuleList<Color>;

/// A computed value for the `column-rule-style` and `row-rule-style`
/// properties.
pub type LineStyleList = GapRuleList<BorderStyle>;

/// A computed value for the `column-rule-width` and `row-rule-width`
/// properties.
pub type LineWidthList = GapRuleList<BorderSideWidth>;

/// A computed value for the rule inset properties.
pub type RuleInset = GenericRuleInset<LengthPercentage>;
