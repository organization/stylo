/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A [`@container`][container] rule.
//!
//! [container]: https://drafts.csswg.org/css-contain-3/#container-rule

use crate::computed_value_flags::ComputedValueFlags;
use crate::dom::{AttributeTracker, TElement};
use crate::logical_geometry::{LogicalSize, PhysicalSide, WritingMode};
use crate::parser::{Parse, ParserContext};
use crate::properties::ComputedValues;
use crate::queries::feature::{AllowsRanges, Evaluator, FeatureFlags, QueryFeatureDescription};
use crate::queries::values::Orientation;
use crate::queries::{FeatureType, QueryCondition};
use crate::selector_map::{PrecomputedHashMap, PrecomputedHashSet};
use crate::shared_lock::{
    DeepCloneWithLock, Locked, SharedRwLock, SharedRwLockReadGuard, ToCssWithGuard,
};
use crate::stylesheets::{CssRules, CustomMediaEvaluator};
use crate::stylist::Stylist;
use crate::values::computed::{CSSPixelLength, ContainerType, Context, Ratio};
use crate::values::specified::ContainerName;
use crate::values::specified::position::{
    PositionArea, PositionAreaAxis, PositionAreaKeyword, PositionTryFallbacksItem,
};
use crate::{LocalName, derives::*};
use app_units::Au;
use cssparser::{Parser, SourceLocation};
use euclid::default::Size2D;
#[cfg(feature = "gecko")]
use malloc_size_of::{MallocSizeOfOps, MallocUnconditionalShallowSizeOf};
use selectors::kleene_value::KleeneValue;
use selectors::matching::ElementSelectorFlags;
use servo_arc::Arc;
use std::fmt::{self, Write};
use style_traits::arc_slice::ArcSlice;
use style_traits::{CssStringWriter, CssWriter, ParseError, StyleParseErrorKind, ToCss};

/// A container rule.
#[derive(Debug, ToShmem)]
pub struct ContainerRule {
    /// The container queries and name.
    pub conditions: ContainerConditions,
    /// The nested rules inside the block.
    pub rules: Arc<Locked<CssRules>>,
    /// The source position where this rule was found.
    pub source_location: SourceLocation,
}

impl ContainerRule {
    /// Measure heap usage.
    #[cfg(feature = "gecko")]
    pub fn size_of(&self, guard: &SharedRwLockReadGuard, ops: &mut MallocSizeOfOps) -> usize {
        // Measurement of other fields may be added later.
        self.rules.unconditional_shallow_size_of(ops)
            + self.rules.read_with(guard).size_of(guard, ops)
    }
}

impl DeepCloneWithLock for ContainerRule {
    fn deep_clone_with_lock(&self, lock: &SharedRwLock, guard: &SharedRwLockReadGuard) -> Self {
        let rules = self.rules.read_with(guard);
        Self {
            conditions: self.conditions.clone(),
            rules: Arc::new(lock.wrap(rules.deep_clone_with_lock(lock, guard))),
            source_location: self.source_location,
        }
    }
}

impl ToCssWithGuard for ContainerRule {
    fn to_css(&self, guard: &SharedRwLockReadGuard, dest: &mut CssStringWriter) -> fmt::Result {
        dest.write_str("@container ")?;
        {
            let mut writer = CssWriter::new(dest);
            self.conditions.to_css(&mut writer)?;
        }
        self.rules.read_with(guard).to_css_block(guard, dest)
    }
}

/// Contains all container conditions for a container rule.
///
/// https://drafts.csswg.org/css-conditional-5/#container-rule
#[derive(Clone, Debug, ToCss, ToShmem)]
#[css(comma)]
pub struct ContainerConditions(#[css(iterable)] pub ArcSlice<ContainerCondition>);

/// A set of attribute names used for invalidation of attr() inside
/// style container queries.
pub type AttrReferenceSet = PrecomputedHashSet<LocalName>;

/// Whether this container uses attr() references and if so whether or not this
/// is a named or unnamed container query. Used for determining which kind of
/// restyle hint we need when traversing invalidation.
#[derive(Debug, Clone, Copy, MallocSizeOf, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub enum ContainerAttributeDependencyKind {
    /// This container does not use attr() references.
    None = 0,
    /// This unnamed container uses attr() references.
    UnnamedContainer = 1,
    /// This named container uses attr() references.
    NamedContainer = 2,
}

impl ContainerAttributeDependencyKind {
    /// A named container that uses attr needs to invalidate all descendants that
    /// are affected by style container queries. If we only have an unnamed
    /// container or no attribute dependencies we keep looking for a condition that
    /// would require us to invalidate more.
    pub fn element_container_dependency_kind<E: TElement>(
        element: E,
        local_name: &LocalName,
        stylist: &Stylist,
    ) -> Self {
        let mut name_kind = ContainerAttributeDependencyKind::None;
        stylist.any_applicable_rule_data(element, |data| {
            let value = data.might_have_attribute_dependency_in_container(local_name);
            name_kind = std::cmp::max(name_kind, value);
            name_kind == ContainerAttributeDependencyKind::NamedContainer
        });
        name_kind
    }
}

/// A container condition and filter, combined.
#[derive(Debug, ToShmem, ToCss)]
pub struct ContainerCondition {
    #[css(skip_if = "ContainerName::is_none")]
    name: ContainerName,
    condition: Option<QueryCondition>,
    #[css(skip)]
    flags: FeatureFlags,
    #[css(skip)]
    attributes_referenced: AttrReferenceSet,
}

/// The result of a successful container query lookup.
pub struct ContainerLookupResult<E> {
    /// The relevant container.
    pub element: E,
    /// The sizing / writing-mode information of the container.
    pub info: ContainerInfo,
    /// The style of the element.
    pub style: Arc<ComputedValues>,
}

fn container_type_axes(ty_: ContainerType, wm: WritingMode) -> FeatureFlags {
    if ty_.intersects(ContainerType::SIZE) {
        FeatureFlags::all_container_axes()
    } else if ty_.intersects(ContainerType::INLINE_SIZE) {
        let physical_axis = if wm.is_vertical() {
            FeatureFlags::CONTAINER_REQUIRES_HEIGHT_AXIS
        } else {
            FeatureFlags::CONTAINER_REQUIRES_WIDTH_AXIS
        };
        FeatureFlags::CONTAINER_REQUIRES_INLINE_AXIS | physical_axis
    } else {
        FeatureFlags::empty()
    }
}

enum TraversalResult<T> {
    InProgress,
    StopTraversal,
    Done(T),
}

fn traverse_container<E, F, R>(
    mut e: E,
    originating_element_style: Option<&ComputedValues>,
    evaluator: F,
) -> Option<(E, R)>
where
    E: TElement,
    F: Fn(E, Option<&ComputedValues>) -> TraversalResult<R>,
{
    if originating_element_style.is_some() {
        match evaluator(e, originating_element_style) {
            TraversalResult::InProgress => {},
            TraversalResult::StopTraversal => return None,
            TraversalResult::Done(result) => return Some((e, result)),
        }
    }
    while let Some(element) = e.traversal_parent() {
        match evaluator(element, None) {
            TraversalResult::InProgress => {},
            TraversalResult::StopTraversal => return None,
            TraversalResult::Done(result) => return Some((element, result)),
        }
        e = element;
    }

    None
}

impl ContainerCondition {
    /// Get the name of this condition.
    #[inline]
    pub fn name(&self) -> &ContainerName {
        &self.name
    }
    /// Get the query condition of this condition
    #[inline]
    pub fn query_condition(&self) -> Option<&QueryCondition> {
        self.condition.as_ref()
    }
    /// Parse a container condition.
    pub fn parse(context: &ParserContext, input: &mut Parser) -> Result<Self, ParseError> {
        let name = input
            .try_parse(|input| ContainerName::parse_for_query(context, input))
            .ok()
            .unwrap_or_else(ContainerName::none);
        let condition = input
            .try_parse(|input| QueryCondition::parse(context, input, FeatureType::Container))
            .ok();
        if condition.is_none() && name.is_none() {
            return Err(ParseError::custom(StyleParseErrorKind::UnspecifiedError));
        }
        let mut attributes_referenced = AttrReferenceSet::default();
        if let Some(c) = condition.as_ref() {
            c.collect_attribute_references(&mut attributes_referenced)
        }
        let flags = condition
            .as_ref()
            .map_or(FeatureFlags::empty(), |c| c.cumulative_flags());
        Ok(Self {
            name,
            condition,
            flags,
            attributes_referenced,
        })
    }

    fn valid_container_info<E>(
        &self,
        potential_container: E,
        originating_element_style: Option<&ComputedValues>,
    ) -> TraversalResult<ContainerLookupResult<E>>
    where
        E: TElement,
    {
        let data;
        let style = match originating_element_style {
            Some(s) => s,
            None => {
                data = match potential_container.borrow_data() {
                    Some(d) => d,
                    None => return TraversalResult::InProgress,
                };
                &**data.styles.primary()
            },
        };
        let wm = style.writing_mode;
        let box_style = style.get_box();

        // Filter by container-type.
        let container_type = *box_style.get_container_type();
        let available_axes = container_type_axes(container_type, wm);
        if !available_axes.contains(self.flags.container_axes()) {
            return TraversalResult::InProgress;
        }

        if self
            .flags
            .contains(FeatureFlags::CONTAINER_REQUIRES_SCROLL_STATE)
            && !container_type.intersects(ContainerType::SCROLL_STATE)
        {
            return TraversalResult::InProgress;
        }

        if self
            .flags
            .contains(FeatureFlags::CONTAINER_REQUIRES_ANCHORED)
            && !container_type.intersects(ContainerType::ANCHORED)
        {
            return TraversalResult::InProgress;
        }

        // Filter by container-name.
        let container_name = box_style.get_container_name();
        for filter_name in self.name.0.iter() {
            if !container_name.0.contains(filter_name) {
                return TraversalResult::InProgress;
            }
        }

        let size = potential_container.query_container_size(box_style.get_display());
        let scroll_state = if container_type.intersects(ContainerType::SCROLL_STATE) {
            potential_container.query_scroll_state()
        } else {
            ContainerScrollState::empty()
        };
        let anchored = container_type
            .intersects(ContainerType::ANCHORED)
            .then(|| potential_container.query_anchored_fallback());
        let style = style.to_arc();
        TraversalResult::Done(ContainerLookupResult {
            element: potential_container,
            info: ContainerInfo {
                size,
                scroll_state,
                anchored,
                wm,
                inherited_style: {
                    potential_container.traversal_parent().and_then(|parent| {
                        parent
                            .borrow_data()
                            .and_then(|data| data.styles.get_primary().cloned())
                    })
                },
            },
            style,
        })
    }

    /// Performs container lookup for a given element.
    pub fn find_container<E>(
        &self,
        e: E,
        originating_element_style: Option<&ComputedValues>,
    ) -> Option<ContainerLookupResult<E>>
    where
        E: TElement,
    {
        traverse_container(
            e,
            originating_element_style,
            |element, originating_element_style| {
                self.valid_container_info(element, originating_element_style)
            },
        )
        .map(|(_, result)| result)
    }

    /// Tries to match a container query condition for a given element.
    pub fn matches<E>(
        &self,
        stylist: &Stylist,
        element: E,
        originating_element_style: Option<&ComputedValues>,
        invalidation_flags: &mut ComputedValueFlags,
    ) -> KleeneValue
    where
        E: TElement,
    {
        // A query with an unknown feature selects no container, so it never
        // matches.
        if self.flags.contains(FeatureFlags::UNKNOWN) {
            return KleeneValue::False;
        }
        let result = self.find_container(element, originating_element_style);
        let condition = match self.condition {
            Some(ref c) => c,
            None => {
                // Condition-less container query (name only): matches if a
                // named container was found.
                return KleeneValue::from(result.is_some());
            },
        };
        // We have to tag the invalidation flags here because style container
        // query matching may return early if we cannot find a suitable
        // container element right now. However, we must also consider the case
        // when an ancestor becomes a container and we have to invalidate this
        // element from not matching to matching.
        if self.flags.contains(FeatureFlags::STYLE) {
            invalidation_flags.insert(ComputedValueFlags::DEPENDS_ON_CONTAINER_STYLE_QUERY);
        }
        let (container, info) = match result {
            Some(r) => (r.element, (r.info, r.style)),
            None => {
                // If we did not find the named (or any) container,
                // the query must fail to match.
                return KleeneValue::False;
            },
        };
        // Set up the lookup for the container in question, as the condition may be using container
        // query lengths.
        let size_query_container_lookup = ContainerSizeQuery::for_element(
            container, /* known_parent_style = */ None, /* is_pseudo = */ false,
        );
        let mut attribute_tracker = AttributeTracker::new(&container);
        Context::for_container_query_evaluation(
            stylist.device(),
            Some(stylist),
            Some(info),
            size_query_container_lookup,
            &container,
            |context| {
                let matches = condition.matches(
                    context,
                    &mut CustomMediaEvaluator::none(),
                    &mut attribute_tracker,
                );
                let flags = context.style().flags();
                if flags.contains(ComputedValueFlags::USES_VIEWPORT_UNITS) {
                    // TODO(emilio): Might need something similar to improve
                    // invalidation of font relative container-query lengths.
                    invalidation_flags
                        .insert(ComputedValueFlags::USES_VIEWPORT_UNITS_ON_CONTAINER_QUERIES);
                }
                if flags.contains(ComputedValueFlags::USES_FONT_OR_WM_RELATIVE_UNITS) {
                    invalidation_flags.insert(
                        ComputedValueFlags::USES_FONT_OR_WM_RELATIVE_UNITS_ON_CONTAINER_QUERIES,
                    );
                }
                if flags.intersects(ComputedValueFlags::tree_counting_function_flags()) {
                    // Container query usage of sibling-index() and sibling-count() requires
                    // redoing selector matches on sibling changes. Although this is not itself
                    // a selector, the HAS_SLOW_SELECTOR flag is reused here because it has the
                    // required invalidation behavior.
                    container.apply_selector_flags(ElementSelectorFlags::HAS_SLOW_SELECTOR);
                }
                matches
            },
        )
    }

    /// Get the attribute functions referenced by this condition, if any.
    pub fn get_attributes_referenced(&self) -> &AttrReferenceSet {
        &self.attributes_referenced
    }

    /// Insert the attribute dependencies and attribute dependency kind into
    /// the attribute_dependencies set and the attr_function_dependencies map.
    pub fn insert_attribute_references_in_dependency_map(
        &self,
        kind_map: &mut PrecomputedHashMap<LocalName, ContainerAttributeDependencyKind>,
        attribute_dependencies: &mut AttrReferenceSet,
    ) {
        let name_kind = if self.name.is_none() {
            ContainerAttributeDependencyKind::UnnamedContainer
        } else {
            ContainerAttributeDependencyKind::NamedContainer
        };
        for attr in self.get_attributes_referenced() {
            kind_map
                .entry_ref(attr)
                .and_modify(|v| {
                    if *v == ContainerAttributeDependencyKind::UnnamedContainer {
                        *v = name_kind
                    }
                })
                .or_insert(name_kind);
            attribute_dependencies.get_or_insert_with(attr, Clone::clone);
        }
    }
}

/// The state of a scroll-state container, in physical directions.
///
/// https://drafts.csswg.org/css-conditional-5/#scroll-state-container
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContainerScrollState(u16);
bitflags! {
    impl ContainerScrollState: u16 {
        /// The container can be scrolled towards its top edge.
        const SCROLLABLE_TOP = 1 << 0;
        /// The container can be scrolled towards its right edge.
        const SCROLLABLE_RIGHT = 1 << 1;
        /// The container can be scrolled towards its bottom edge.
        const SCROLLABLE_BOTTOM = 1 << 2;
        /// The container can be scrolled towards its left edge.
        const SCROLLABLE_LEFT = 1 << 3;
        /// The most recent relative vertical scroll went towards the top.
        const SCROLLED_TOP = 1 << 4;
        /// The most recent relative horizontal scroll went towards the right.
        const SCROLLED_RIGHT = 1 << 5;
        /// The most recent relative vertical scroll went towards the bottom.
        const SCROLLED_BOTTOM = 1 << 6;
        /// The most recent relative horizontal scroll went towards the left.
        const SCROLLED_LEFT = 1 << 7;
        /// The sticky container is stuck to the top edge of its scroller.
        const STUCK_TOP = 1 << 8;
        /// The sticky container is stuck to the right edge of its scroller.
        const STUCK_RIGHT = 1 << 9;
        /// The sticky container is stuck to the bottom edge of its scroller.
        const STUCK_BOTTOM = 1 << 10;
        /// The sticky container is stuck to the left edge of its scroller.
        const STUCK_LEFT = 1 << 11;
        /// The container is snapped in the horizontal axis.
        const SNAPPED_X = 1 << 12;
        /// The container is snapped in the vertical axis.
        const SNAPPED_Y = 1 << 13;
    }
}

const SCROLLABLE_SHIFT: u16 = 0;
const SCROLLED_SHIFT: u16 = 4;
const STUCK_SHIFT: u16 = 8;

/// Information needed to evaluate an individual container query.
#[derive(Clone)]
pub struct ContainerInfo {
    size: Size2D<Option<Au>>,
    scroll_state: ContainerScrollState,
    anchored: Option<(WritingMode, Option<PositionTryFallbacksItem>)>,
    wm: WritingMode,
    inherited_style: Option<Arc<ComputedValues>>,
}

impl ContainerInfo {
    fn size(&self) -> Option<Size2D<Au>> {
        Some(Size2D::new(self.size.width?, self.size.height?))
    }

    /// Get a reference to the container's inherited style, if any.
    pub fn inherited_style(&self) -> Option<&ComputedValues> {
        self.inherited_style.as_deref()
    }
}

fn eval_width(context: &Context) -> Option<CSSPixelLength> {
    let info = context.container_info.as_ref()?;
    Some(CSSPixelLength::new(info.size.width?.to_f32_px()))
}

fn eval_height(context: &Context) -> Option<CSSPixelLength> {
    let info = context.container_info.as_ref()?;
    Some(CSSPixelLength::new(info.size.height?.to_f32_px()))
}

fn eval_inline_size(context: &Context) -> Option<CSSPixelLength> {
    let info = context.container_info.as_ref()?;
    Some(CSSPixelLength::new(
        LogicalSize::from_physical(info.wm, info.size)
            .inline?
            .to_f32_px(),
    ))
}

fn eval_block_size(context: &Context) -> Option<CSSPixelLength> {
    let info = context.container_info.as_ref()?;
    Some(CSSPixelLength::new(
        LogicalSize::from_physical(info.wm, info.size)
            .block?
            .to_f32_px(),
    ))
}

fn eval_aspect_ratio(context: &Context) -> Option<Ratio> {
    let info = context.container_info.as_ref()?;
    Some(Ratio::new(
        info.size.width?.0 as f32,
        info.size.height?.0 as f32,
    ))
}

fn eval_orientation(context: &Context, value: Option<Orientation>) -> KleeneValue {
    let size = match context.container_info.as_ref().and_then(|info| info.size()) {
        Some(size) => size,
        None => return KleeneValue::Unknown,
    };
    KleeneValue::from(Orientation::eval(size, value))
}

/// https://drafts.csswg.org/css-contain-3/#container-features
///
/// TODO: Support style queries, perhaps.
pub static CONTAINER_FEATURES: [QueryFeatureDescription; 6] = [
    feature!(
        atom!("width"),
        AllowsRanges::Yes,
        Evaluator::OptionalLength(eval_width),
        FeatureFlags::CONTAINER_REQUIRES_WIDTH_AXIS,
    ),
    feature!(
        atom!("height"),
        AllowsRanges::Yes,
        Evaluator::OptionalLength(eval_height),
        FeatureFlags::CONTAINER_REQUIRES_HEIGHT_AXIS,
    ),
    feature!(
        atom!("inline-size"),
        AllowsRanges::Yes,
        Evaluator::OptionalLength(eval_inline_size),
        FeatureFlags::CONTAINER_REQUIRES_INLINE_AXIS,
    ),
    feature!(
        atom!("block-size"),
        AllowsRanges::Yes,
        Evaluator::OptionalLength(eval_block_size),
        FeatureFlags::CONTAINER_REQUIRES_BLOCK_AXIS,
    ),
    feature!(
        atom!("aspect-ratio"),
        AllowsRanges::Yes,
        Evaluator::OptionalNumberRatio(eval_aspect_ratio),
        // XXX from_bits_truncate is const, but the pipe operator isn't, so this
        // works around it.
        FeatureFlags::from_bits_truncate(
            FeatureFlags::CONTAINER_REQUIRES_BLOCK_AXIS.bits()
                | FeatureFlags::CONTAINER_REQUIRES_INLINE_AXIS.bits()
        ),
    ),
    feature!(
        atom!("orientation"),
        AllowsRanges::No,
        keyword_evaluator!(eval_orientation, Orientation),
        FeatureFlags::from_bits_truncate(
            FeatureFlags::CONTAINER_REQUIRES_BLOCK_AXIS.bits()
                | FeatureFlags::CONTAINER_REQUIRES_INLINE_AXIS.bits()
        ),
    ),
];

/// The value of the `scrollable` and `scrolled` scroll-state features.
#[derive(Clone, Copy, Debug, FromPrimitive, Parse, ToCss)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum ScrollStateDirection {
    None,
    Top,
    Right,
    Bottom,
    Left,
    BlockStart,
    InlineStart,
    BlockEnd,
    InlineEnd,
    X,
    Y,
    Block,
    Inline,
}

/// The value of the `stuck` scroll-state feature.
#[derive(Clone, Copy, Debug, FromPrimitive, Parse, ToCss)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum ScrollStateStuck {
    None,
    Top,
    Right,
    Bottom,
    Left,
    BlockStart,
    InlineStart,
    BlockEnd,
    InlineEnd,
}

/// The value of the `snapped` scroll-state feature.
#[derive(Clone, Copy, Debug, FromPrimitive, Parse, ToCss)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum ScrollStateSnapped {
    None,
    X,
    Y,
    Block,
    Inline,
    Both,
}

fn scroll_state_sides(context: &Context, shift: u16) -> Option<(u16, WritingMode)> {
    let info = context.container_info.as_ref()?;
    Some(((info.scroll_state.bits() >> shift) & 0b1111, info.wm))
}

fn eval_sides(context: &Context, shift: u16, value: Option<ScrollStateDirection>) -> bool {
    use ScrollStateDirection::*;
    let Some((sides, wm)) = scroll_state_sides(context, shift) else {
        return false;
    };
    let side = |side: PhysicalSide| sides & (1 << side as u16) != 0;
    let Some(value) = value else {
        return sides != 0;
    };
    match value {
        None => sides == 0,
        Top => side(PhysicalSide::Top),
        Right => side(PhysicalSide::Right),
        Bottom => side(PhysicalSide::Bottom),
        Left => side(PhysicalSide::Left),
        BlockStart => side(wm.block_start_physical_side()),
        InlineStart => side(wm.inline_start_physical_side()),
        BlockEnd => side(wm.block_end_physical_side()),
        InlineEnd => side(wm.inline_end_physical_side()),
        X => side(PhysicalSide::Left) || side(PhysicalSide::Right),
        Y => side(PhysicalSide::Top) || side(PhysicalSide::Bottom),
        Block => side(wm.block_start_physical_side()) || side(wm.block_end_physical_side()),
        Inline => side(wm.inline_start_physical_side()) || side(wm.inline_end_physical_side()),
    }
}

fn eval_scrollable(context: &Context, value: Option<ScrollStateDirection>) -> bool {
    eval_sides(context, SCROLLABLE_SHIFT, value)
}

fn eval_scrolled(context: &Context, value: Option<ScrollStateDirection>) -> bool {
    eval_sides(context, SCROLLED_SHIFT, value)
}

fn eval_stuck(context: &Context, value: Option<ScrollStateStuck>) -> bool {
    let value = value.map(|value| match value {
        ScrollStateStuck::None => ScrollStateDirection::None,
        ScrollStateStuck::Top => ScrollStateDirection::Top,
        ScrollStateStuck::Right => ScrollStateDirection::Right,
        ScrollStateStuck::Bottom => ScrollStateDirection::Bottom,
        ScrollStateStuck::Left => ScrollStateDirection::Left,
        ScrollStateStuck::BlockStart => ScrollStateDirection::BlockStart,
        ScrollStateStuck::InlineStart => ScrollStateDirection::InlineStart,
        ScrollStateStuck::BlockEnd => ScrollStateDirection::BlockEnd,
        ScrollStateStuck::InlineEnd => ScrollStateDirection::InlineEnd,
    });
    eval_sides(context, STUCK_SHIFT, value)
}

fn eval_snapped(context: &Context, value: Option<ScrollStateSnapped>) -> bool {
    let Some(info) = context.container_info.as_ref() else {
        return false;
    };
    let x = info.scroll_state.contains(ContainerScrollState::SNAPPED_X);
    let y = info.scroll_state.contains(ContainerScrollState::SNAPPED_Y);
    let (inline, block) = if info.wm.is_vertical() {
        (y, x)
    } else {
        (x, y)
    };
    match value {
        Option::None => x || y,
        Some(ScrollStateSnapped::None) => !x && !y,
        Some(ScrollStateSnapped::X) => x,
        Some(ScrollStateSnapped::Y) => y,
        Some(ScrollStateSnapped::Block) => block,
        Some(ScrollStateSnapped::Inline) => inline,
        Some(ScrollStateSnapped::Both) => x && y,
    }
}

/// The value of the `fallback` feature of an `anchored()` query.
///
/// https://drafts.csswg.org/css-anchor-position-2/#anchored-container-queries
#[derive(Clone, Debug, MallocSizeOf, PartialEq, ToCss, ToShmem)]
pub enum AnchoredFallback {
    /// `none`: the container uses its base styles.
    None,
    /// A position option of `position-try-fallbacks`.
    Item(PositionTryFallbacksItem),
}

impl Parse for AnchoredFallback {
    fn parse(context: &ParserContext, input: &mut Parser) -> Result<Self, ParseError> {
        if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
            return Ok(Self::None);
        }
        Ok(Self::Item(PositionTryFallbacksItem::parse(context, input)?))
    }
}

fn eval_fallback(context: &Context, value: Option<&AnchoredFallback>) -> bool {
    let Some(info) = context.container_info.as_ref() else {
        return false;
    };
    let Some((cb_wm, ref applied)) = info.anchored else {
        return false;
    };
    match (value, applied) {
        (None, applied) => applied.is_some(),
        (Some(AnchoredFallback::None), applied) => applied.is_none(),
        (
            Some(AnchoredFallback::Item(PositionTryFallbacksItem::PositionArea(queried))),
            Some(PositionTryFallbacksItem::PositionArea(applied)),
        ) => physical_tracks(*queried, cb_wm, info.wm) == physical_tracks(*applied, cb_wm, info.wm),
        (Some(AnchoredFallback::Item(item)), applied) => applied.as_ref() == Some(item),
    }
}

fn physical_tracks(area: PositionArea, cb_wm: WritingMode, wm: WritingMode) -> [Option<u8>; 2] {
    let area = area.to_physical(cb_wm, wm);
    let track = |keyword: PositionAreaKeyword| keyword.track().map(|track| track as u8);
    if area.first.axis() == PositionAreaAxis::Vertical
        || area.second.axis() == PositionAreaAxis::Horizontal
    {
        [track(area.second), track(area.first)]
    } else {
        [track(area.first), track(area.second)]
    }
}

/// https://drafts.csswg.org/css-anchor-position-2/#anchored-container-queries
pub static ANCHORED_FEATURES: [QueryFeatureDescription; 1] = [feature!(
    atom!("fallback"),
    AllowsRanges::No,
    Evaluator::AnchoredFallback(eval_fallback),
    FeatureFlags::CONTAINER_REQUIRES_ANCHORED,
)];

/// https://drafts.csswg.org/css-conditional-5/#scroll-state-container
pub static SCROLL_STATE_FEATURES: [QueryFeatureDescription; 4] = [
    feature!(
        atom!("stuck"),
        AllowsRanges::No,
        keyword_evaluator!(eval_stuck, ScrollStateStuck),
        FeatureFlags::CONTAINER_REQUIRES_SCROLL_STATE,
    ),
    feature!(
        atom!("snapped"),
        AllowsRanges::No,
        keyword_evaluator!(eval_snapped, ScrollStateSnapped),
        FeatureFlags::CONTAINER_REQUIRES_SCROLL_STATE,
    ),
    feature!(
        atom!("scrollable"),
        AllowsRanges::No,
        keyword_evaluator!(eval_scrollable, ScrollStateDirection),
        FeatureFlags::CONTAINER_REQUIRES_SCROLL_STATE,
    ),
    feature!(
        atom!("scrolled"),
        AllowsRanges::No,
        keyword_evaluator!(eval_scrolled, ScrollStateDirection),
        FeatureFlags::CONTAINER_REQUIRES_SCROLL_STATE,
    ),
];

/// Result of a container size query, signifying the hypothetical containment boundary in terms of physical axes.
/// Defined by up to two size containers. Queries on logical axes are resolved with respect to the querying
/// element's writing mode.
#[derive(Copy, Clone, Default)]
pub struct ContainerSizeQueryResult {
    width: Option<Au>,
    height: Option<Au>,
}

impl ContainerSizeQueryResult {
    fn get_viewport_size(context: &Context) -> Size2D<Au> {
        use crate::values::specified::ViewportVariant;
        context.viewport_size_for_viewport_unit_resolution(ViewportVariant::Small)
    }

    fn get_logical_viewport_size(context: &Context) -> LogicalSize<Au> {
        LogicalSize::from_physical(
            context.builder.writing_mode,
            Self::get_viewport_size(context),
        )
    }

    /// Get the inline-size of the query container.
    pub fn get_container_inline_size(&self, context: &Context) -> Au {
        if context.builder.writing_mode.is_horizontal() {
            if let Some(w) = self.width {
                return w;
            }
        } else if let Some(h) = self.height {
            return h;
        }
        Self::get_logical_viewport_size(context).inline
    }

    /// Get the block-size of the query container.
    pub fn get_container_block_size(&self, context: &Context) -> Au {
        if context.builder.writing_mode.is_horizontal() {
            self.get_container_height(context)
        } else {
            self.get_container_width(context)
        }
    }

    /// Get the width of the query container.
    pub fn get_container_width(&self, context: &Context) -> Au {
        if let Some(w) = self.width {
            return w;
        }
        Self::get_viewport_size(context).width
    }

    /// Get the height of the query container.
    pub fn get_container_height(&self, context: &Context) -> Au {
        if let Some(h) = self.height {
            return h;
        }
        Self::get_viewport_size(context).height
    }

    // Merge the result of a subsequent lookup, preferring the initial result.
    fn merge(self, new_result: Self) -> Self {
        let mut result = self;
        if let Some(width) = new_result.width {
            result.width.get_or_insert(width);
        }
        if let Some(height) = new_result.height {
            result.height.get_or_insert(height);
        }
        result
    }

    fn is_complete(&self) -> bool {
        self.width.is_some() && self.height.is_some()
    }
}

/// Unevaluated lazy container size query.
pub enum ContainerSizeQuery<'a> {
    /// Query prior to evaluation.
    NotEvaluated(Box<dyn Fn() -> ContainerSizeQueryResult + 'a>),
    /// Cached evaluated result.
    Evaluated(ContainerSizeQueryResult),
}

impl<'a> ContainerSizeQuery<'a> {
    fn evaluate_potential_size_container<E>(
        e: E,
        originating_element_style: Option<&ComputedValues>,
    ) -> TraversalResult<ContainerSizeQueryResult>
    where
        E: TElement,
    {
        let data;
        let style = match originating_element_style {
            Some(s) => s,
            None => {
                data = match e.borrow_data() {
                    Some(d) => d,
                    None => return TraversalResult::InProgress,
                };
                &**data.styles.primary()
            },
        };
        if !style
            .flags
            .contains(ComputedValueFlags::SELF_OR_ANCESTOR_HAS_SIZE_CONTAINER_TYPE)
        {
            // We know we won't find a size container.
            return TraversalResult::StopTraversal;
        }

        let wm = style.writing_mode;
        let box_style = style.get_box();

        let container_type = *box_style.get_container_type();
        let size = e.query_container_size(box_style.get_display());
        if container_type.intersects(ContainerType::SIZE) {
            TraversalResult::Done(ContainerSizeQueryResult {
                width: size.width,
                height: size.height,
            })
        } else if container_type.intersects(ContainerType::INLINE_SIZE) {
            if wm.is_horizontal() {
                TraversalResult::Done(ContainerSizeQueryResult {
                    width: size.width,
                    height: None,
                })
            } else {
                TraversalResult::Done(ContainerSizeQueryResult {
                    width: None,
                    height: size.height,
                })
            }
        } else {
            TraversalResult::InProgress
        }
    }

    /// Find the query container size for a given element. Meant to be used as a callback for new().
    fn lookup<E>(
        element: E,
        originating_element_style: Option<&ComputedValues>,
    ) -> ContainerSizeQueryResult
    where
        E: TElement + 'a,
    {
        match traverse_container(
            element,
            originating_element_style,
            |e, originating_element_style| {
                Self::evaluate_potential_size_container(e, originating_element_style)
            },
        ) {
            Some((container, result)) => {
                if result.is_complete() {
                    result
                } else {
                    // Traverse up from the found size container to see if we can get a complete containment.
                    result.merge(Self::lookup(container, None))
                }
            },
            None => ContainerSizeQueryResult::default(),
        }
    }

    /// Create a new instance of the container size query for given element, with a deferred lookup callback.
    pub fn for_element<E>(
        element: E,
        known_parent_style: Option<&'a ComputedValues>,
        is_pseudo: bool,
    ) -> Self
    where
        E: TElement + 'a,
    {
        let parent;
        let data;
        let parent_style = match known_parent_style {
            Some(s) => Some(s),
            None => {
                // No need to bother if we're the top element.
                parent = match element.traversal_parent() {
                    Some(parent) => parent,
                    None => return Self::none(),
                };
                data = parent.borrow_data();
                data.as_ref().map(|data| &**data.styles.primary())
            },
        };

        // If there's no style, such as being `display: none` or so, we still want to show a
        // correct computed value, so give it a try.
        let should_traverse = parent_style.is_none_or(|s| {
            s.flags
                .contains(ComputedValueFlags::SELF_OR_ANCESTOR_HAS_SIZE_CONTAINER_TYPE)
        });
        if !should_traverse {
            return Self::none();
        }
        Self::NotEvaluated(Box::new(move || {
            Self::lookup(element, if is_pseudo { known_parent_style } else { None })
        }))
    }

    /// Create a new instance, but with optional element.
    pub fn for_option_element<E>(
        element: Option<E>,
        known_parent_style: Option<&'a ComputedValues>,
        is_pseudo: bool,
    ) -> Self
    where
        E: TElement + 'a,
    {
        if let Some(e) = element {
            Self::for_element(e, known_parent_style, is_pseudo)
        } else {
            Self::none()
        }
    }

    /// Create a query that evaluates to empty, for cases where container size query is not required.
    pub fn none() -> Self {
        ContainerSizeQuery::Evaluated(ContainerSizeQueryResult::default())
    }

    /// Get the result of the container size query, doing the lookup if called for the first time.
    pub fn get(&mut self) -> ContainerSizeQueryResult {
        match self {
            Self::NotEvaluated(lookup) => {
                *self = Self::Evaluated((lookup)());
                match self {
                    Self::Evaluated(info) => *info,
                    _ => unreachable!("Just evaluated but not set?"),
                }
            },
            Self::Evaluated(info) => *info,
        }
    }
}
