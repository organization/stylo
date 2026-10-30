/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Generic types for the column properties.
use crate::derives::*;

/// A generic type for `column-count` values.
#[derive(
    Animate,
    Clone,
    ComputeSquaredDistance,
    Copy,
    Debug,
    MallocSizeOf,
    Parse,
    PartialEq,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToAnimatedZero,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
    ToTyped,
)]
#[repr(u8)]
#[typed(todo_derive_fields)]
pub enum GenericColumnCount<PositiveInteger> {
    /// A positive integer.
    Integer(PositiveInteger),
    /// The keyword `auto`.
    #[animation(error)]
    Auto,
}

pub use self::GenericColumnCount as ColumnCount;
impl<I> ColumnCount<I> {
    /// Returns whether this value is `auto`.
    #[inline]
    pub fn is_auto(&self) -> bool {
        matches!(self, ColumnCount::Auto)
    }
}

/// The repeat count of a `repeat()` in a gap decoration list.
#[derive(
    Clone,
    Copy,
    Debug,
    MallocSizeOf,
    PartialEq,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
)]
#[repr(C, u8)]
pub enum GenericGapRepeatCount<PositiveInteger> {
    /// A positive integer.
    Integer(PositiveInteger),
    /// The keyword `auto`.
    Auto,
}

pub use self::GenericGapRepeatCount as GapRepeatCount;

/// A `repeat()` of gap decoration values.
///
/// https://drafts.csswg.org/css-gaps-1/#typedef-repeat-line-color
#[derive(
    Clone,
    Debug,
    MallocSizeOf,
    PartialEq,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
)]
#[css(comma, function = "repeat")]
#[repr(C)]
pub struct GenericGapRepeat<T, PositiveInteger> {
    /// The number of repetitions, or `auto`.
    pub count: GenericGapRepeatCount<PositiveInteger>,
    /// The repeated values.
    #[css(iterable)]
    pub values: crate::OwnedSlice<T>,
}

pub use self::GenericGapRepeat as GapRepeat;

/// A single value or a `repeat()` in a gap decoration list.
#[derive(
    Clone,
    Debug,
    MallocSizeOf,
    PartialEq,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
)]
#[repr(C, u8)]
pub enum GenericGapRuleItem<T, PositiveInteger> {
    /// A single value.
    Value(T),
    /// A `repeat()` of values.
    Repeat(GenericGapRepeat<T, PositiveInteger>),
}

pub use self::GenericGapRuleItem as GapRuleItem;

/// A comma-separated list of gap decoration values, used by the
/// `column-rule-*` and `row-rule-*` color, style and width properties.
///
/// https://drafts.csswg.org/css-gaps-1/#color
#[derive(
    Clone,
    Debug,
    MallocSizeOf,
    PartialEq,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
)]
#[css(comma)]
#[repr(C)]
pub struct GenericGapRuleList<T, PositiveInteger>(
    #[css(iterable)] pub crate::OwnedSlice<GenericGapRuleItem<T, PositiveInteger>>,
);

pub use self::GenericGapRuleList as GapRuleList;

impl<T, I> GapRuleList<T, I> {
    /// Returns a list with a single value.
    pub fn single(value: T) -> Self {
        Self(crate::OwnedSlice::from(vec![GapRuleItem::Value(value)]))
    }

    /// Returns a list of the same shape, with every value mapped by `f`.
    pub fn map<U>(&self, mut f: impl FnMut(&T) -> U) -> GapRuleList<U, I>
    where
        I: Clone,
    {
        GapRuleList(
            self.0
                .iter()
                .map(|item| match item {
                    GapRuleItem::Value(value) => GapRuleItem::Value(f(value)),
                    GapRuleItem::Repeat(repeat) => GapRuleItem::Repeat(GapRepeat {
                        count: repeat.count.clone(),
                        values: repeat.values.iter().map(&mut f).collect(),
                    }),
                })
                .collect(),
        )
    }
}

impl<T, I> crate::typed_om::ToTyped for GapRuleList<T, I> {}

/// A `<length-percentage> | overlap-join` value of the rule inset properties.
///
/// https://drafts.csswg.org/css-gaps-1/#typedef-inset-value
#[derive(
    Animate,
    Clone,
    ComputeSquaredDistance,
    Debug,
    MallocSizeOf,
    Parse,
    PartialEq,
    SpecifiedValueInfo,
    ToAnimatedValue,
    ToAnimatedZero,
    ToComputedValue,
    ToCss,
    ToResolvedValue,
    ToShmem,
    ToTyped,
)]
#[repr(C, u8)]
#[typed(todo_derive_fields)]
pub enum GenericRuleInset<LengthPercentage> {
    /// A length or percentage.
    LengthPercentage(LengthPercentage),
    /// The keyword `overlap-join`.
    #[animation(error)]
    OverlapJoin,
}

pub use self::GenericRuleInset as RuleInset;

impl<L: crate::Zero> RuleInset<L> {
    /// Returns the initial value, `0`.
    pub fn zero() -> Self {
        Self::LengthPercentage(L::zero())
    }
}
