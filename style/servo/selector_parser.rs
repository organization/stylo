/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![deny(missing_docs)]

//! Servo's selector parser.

use crate::FxHashMap;
use crate::attr::{AttrIdentifier, AttrValue};
use crate::computed_value_flags::ComputedValueFlags;
use crate::derives::*;
use crate::dom::{OpaqueNode, TElement, TNode};
use crate::invalidation::element::document_state::InvalidationMatchingData;
use crate::invalidation::element::element_wrapper::ElementSnapshot;
use crate::properties::longhands::display::computed_value::T as Display;
use crate::properties::{ComputedValues, PropertyFlags};
use crate::selector_parser::AttrValue as SelectorAttrValue;
use crate::selector_parser::{Direction, PseudoElementCascadeType, SelectorParser};
use crate::values::{AtomIdent, AtomString, CSSInteger};
use crate::{Atom, CaseSensitivityExt, LocalName, Namespace, Prefix};
use cssparser::{Parser as CssParser, ToCss, match_ignore_ascii_case, serialize_identifier};
use dom::{DocumentState, ElementState};
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::parser::SelectorParseErrorKind;
use std::fmt;
use std::ops::{Deref, DerefMut};
use style_traits::{ParseError, StyleParseErrorKind};

/// A pseudo-element, both public and private.
///
/// NB: If you add to this list, be sure to update `each_simple_pseudo_element` too.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, MallocSizeOf, PartialEq, Serialize, ToShmem,
)]
#[allow(missing_docs)]
#[repr(u8)]
pub enum PseudoElement {
    // Eager pseudos. Keep these first so that eager_index() works.
    After = 0,
    Before,
    Selection,
    // Make sure the UA sheet has the !important rules some of the
    // APPLIES_TO_PLACEHOLDER properties expect!
    FirstLetter,
    FirstLine,

    // Non-eager pseudos.
    Backdrop,
    DetailsContent,
    InterestButton,
    Marker,

    // Implemented pseudos. These pseudo elements are representing the
    // elements within an UA shadow DOM, and matching the elements with
    // their appropriate styles.
    ColorSwatch,
    FileSelectorButton,
    Placeholder,
    SliderFill,
    SliderThumb,
    SliderTrack,
    MozProgressBar,

    // Private, Servo-specific implemented pseudos. Only matchable in UA sheet.
    ServoTextControlInnerContainer,
    ServoTextControlInnerEditor,

    // Other Servo-specific pseudos.
    ServoAnonymousBox,
    ServoAnonymousTable,
    ServoAnonymousTableCell,
    ServoAnonymousTableRow,
    ServoTableGrid,
    ServoTableWrapper,

    // View transition pseudos, which are parsed but never generated.
    ViewTransition,
    ViewTransitionGroup(PtNameAndClassSelector),
    ViewTransitionImagePair(PtNameAndClassSelector),
    ViewTransitionOld(PtNameAndClassSelector),
    ViewTransitionNew(PtNameAndClassSelector),

    // Pseudo-elements that are parsed but never generated.
    TargetText,
    SpellingError,
    GrammarError,
    SearchText,
    Picker,
    PickerIcon,
    Checkmark,
    ScrollMarker,
    ScrollMarkerGroup,
    ScrollButton(ScrollButtonDirection),
    Column,

    // A `::-webkit-*` pseudo-element that is not known, which is valid but never matches.
    UnknownWebkit(UnknownWebkitName),
    Highlight(HighlightName),
}

/// The lowercased name of an unknown `::-webkit-*` pseudo-element after the prefix, interned so
/// that [`PseudoElement`] stays `Copy`.
///
/// <https://drafts.csswg.org/selectors-4/#compat>
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, MallocSizeOf, PartialEq, Serialize, ToShmem,
)]
pub struct UnknownWebkitName(u32);

static UNKNOWN_WEBKIT_NAMES: std::sync::RwLock<Vec<Atom>> = std::sync::RwLock::new(Vec::new());

impl UnknownWebkitName {
    fn intern(name: Atom) -> Self {
        let mut names = UNKNOWN_WEBKIT_NAMES.write().unwrap();
        let index = match names.iter().position(|existing| *existing == name) {
            Some(index) => index,
            None => {
                names.push(name);
                names.len() - 1
            },
        };
        Self(index as u32)
    }
}

/// The argument of `::scroll-button()`.
///
/// <https://drafts.csswg.org/css-overflow-5/#scroll-buttons>
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, MallocSizeOf, PartialEq, Serialize, ToShmem,
)]
#[allow(missing_docs)]
pub enum ScrollButtonDirection {
    Any,
    Up,
    Down,
    Left,
    Right,
    BlockStart,
    BlockEnd,
    InlineStart,
    InlineEnd,
}

impl ScrollButtonDirection {
    fn parse<'i>(input: &mut CssParser<'i>) -> Result<Self, ParseError> {
        if input.try_parse(|i| i.expect_delim('*')).is_ok() {
            return Ok(Self::Any);
        }
        let ident = input.expect_ident()?;
        Ok(match_ignore_ascii_case! { &ident,
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "block-start" => Self::BlockStart,
            "block-end" => Self::BlockEnd,
            "inline-start" => Self::InlineStart,
            "inline-end" => Self::InlineEnd,
            _ => return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent)),
        })
    }

    fn as_str(&self) -> &'static str {
        match *self {
            Self::Any => "*",
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
            Self::BlockStart => "block-start",
            Self::BlockEnd => "block-end",
            Self::InlineStart => "inline-start",
            Self::InlineEnd => "inline-end",
        }
    }
}

/// The count of all pseudo-elements.
pub const PSEUDO_COUNT: usize = 40;

/// The name of a `::highlight()` pseudo-element, interned so that
/// [`PseudoElement`] stays `Copy`.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, MallocSizeOf, PartialEq, Serialize, ToShmem,
)]
pub struct HighlightName(u32);

static HIGHLIGHT_NAMES: std::sync::RwLock<Vec<Atom>> = std::sync::RwLock::new(Vec::new());

impl HighlightName {
    fn intern(name: Atom) -> Self {
        let mut names = HIGHLIGHT_NAMES.write().unwrap();
        let index = match names.iter().position(|existing| *existing == name) {
            Some(index) => index,
            None => {
                names.push(name);
                names.len() - 1
            },
        };
        Self(index as u32)
    }

    /// The name of the highlight.
    pub fn name(&self) -> Atom {
        HIGHLIGHT_NAMES.read().unwrap()[self.0 as usize].clone()
    }
}

/// The argument of the view transition pseudo-elements, interned so that
/// [`PseudoElement`] stays `Copy`.
///
/// <https://drafts.csswg.org/css-view-transitions-2/#typedef-pt-name-and-class-selector>
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, MallocSizeOf, PartialEq, Serialize, ToShmem,
)]
pub struct PtNameAndClassSelector(u32);

static PT_NAMES_AND_CLASSES: std::sync::RwLock<Vec<(Option<Atom>, Vec<Atom>)>> =
    std::sync::RwLock::new(Vec::new());

impl PtNameAndClassSelector {
    fn intern(name: Option<Atom>, classes: Vec<Atom>) -> Self {
        let entry = (name, classes);
        let mut names = PT_NAMES_AND_CLASSES.write().unwrap();
        let index = match names.iter().position(|existing| *existing == entry) {
            Some(index) => index,
            None => {
                names.push(entry);
                names.len() - 1
            },
        };
        Self(index as u32)
    }

    /// `<pt-name-selector> <pt-class-selector>? | <pt-class-selector>`, where
    /// `<pt-name-selector> = '*' | <custom-ident>` and
    /// `<pt-class-selector> = ['.' <custom-ident>]+`.
    fn parse<'i>(input: &mut CssParser<'i>) -> Result<Self, ParseError> {
        use crate::values::CustomIdent;
        use cssparser::Token;

        let name = input.try_parse(|input| {
            if input.try_parse(|i| i.expect_delim('*')).is_ok() {
                Ok(None)
            } else {
                CustomIdent::parse(input, &[]).map(|ident| Some(ident.0))
            }
        });
        if name.is_err() {
            input.skip_whitespace();
        }
        let mut classes = Vec::new();
        while let Ok(class) = input.try_parse(|input| {
            match input.next_including_whitespace()? {
                Token::Delim('.') => {},
                _ => return Err(ParseError::unexpected_token()),
            }
            if input.try_parse(|i| i.expect_whitespace()).is_ok() {
                return Err(ParseError::unexpected_token());
            }
            CustomIdent::parse(input, &[]).map(|ident| ident.0)
        }) {
            classes.push(class);
        }
        if name.is_err() && classes.is_empty() {
            return Err(ParseError::custom(StyleParseErrorKind::UnspecifiedError));
        }
        Ok(Self::intern(name.unwrap_or(None), classes))
    }
}

impl ToCss for PtNameAndClassSelector {
    fn to_css<W>(&self, dest: &mut W) -> fmt::Result
    where
        W: fmt::Write,
    {
        let names = PT_NAMES_AND_CLASSES.read().unwrap();
        let (name, classes) = &names[self.0 as usize];
        match name {
            Some(name) => serialize_identifier(name, dest)?,
            None => dest.write_char('*')?,
        }
        for class in classes {
            dest.write_char('.')?;
            serialize_identifier(class, dest)?;
        }
        Ok(())
    }
}

impl ToCss for PseudoElement {
    fn to_css<W>(&self, dest: &mut W) -> fmt::Result
    where
        W: fmt::Write,
    {
        use self::PseudoElement::*;
        dest.write_str(match *self {
            After => "::after",
            Before => "::before",
            Selection => "::selection",
            FirstLetter => "::first-letter",
            FirstLine => "::first-line",
            Backdrop => "::backdrop",
            DetailsContent => "::details-content",
            InterestButton => "::interest-button",
            Marker => "::marker",
            ColorSwatch => "::color-swatch",
            FileSelectorButton => "::file-selector-button",
            Placeholder => "::placeholder",
            SliderFill => "::slider-fill",
            SliderTrack => "::slider-track",
            SliderThumb => "::slider-thumb",
            MozProgressBar => "::-moz-progress-bar",
            ServoTextControlInnerContainer => "::-servo-text-control-inner-container",
            ServoTextControlInnerEditor => "::-servo-text-control-inner-editor",
            ServoAnonymousBox => "::-servo-anonymous-box",
            ServoAnonymousTable => "::-servo-anonymous-table",
            ServoAnonymousTableCell => "::-servo-anonymous-table-cell",
            ServoAnonymousTableRow => "::-servo-anonymous-table-row",
            ServoTableGrid => "::-servo-table-grid",
            ServoTableWrapper => "::-servo-table-wrapper",
            ViewTransition => "::view-transition",
            ViewTransitionGroup(argument)
            | ViewTransitionImagePair(argument)
            | ViewTransitionOld(argument)
            | ViewTransitionNew(argument) => {
                dest.write_str(match *self {
                    ViewTransitionGroup(..) => "::view-transition-group(",
                    ViewTransitionImagePair(..) => "::view-transition-image-pair(",
                    ViewTransitionOld(..) => "::view-transition-old(",
                    _ => "::view-transition-new(",
                })?;
                argument.to_css(dest)?;
                return dest.write_char(')');
            },
            TargetText => "::target-text",
            SpellingError => "::spelling-error",
            GrammarError => "::grammar-error",
            SearchText => "::search-text",
            Picker => "::picker(select)",
            PickerIcon => "::picker-icon",
            Checkmark => "::checkmark",
            ScrollMarker => "::scroll-marker",
            ScrollMarkerGroup => "::scroll-marker-group",
            ScrollButton(direction) => {
                dest.write_str("::scroll-button(")?;
                dest.write_str(direction.as_str())?;
                return dest.write_char(')');
            },
            Column => "::column",
            UnknownWebkit(name) => {
                dest.write_str("::-webkit-")?;
                let names = UNKNOWN_WEBKIT_NAMES.read().unwrap();
                return serialize_identifier(&names[name.0 as usize], dest);
            },
            Highlight(name) => {
                dest.write_str("::highlight(")?;
                serialize_identifier(&name.name(), dest)?;
                return dest.write_char(')');
            },
        })
    }
}

impl ::selectors::parser::PseudoElement for PseudoElement {
    fn parses_as_element_backed(&self) -> bool {
        matches!(
            self,
            Self::DetailsContent | Self::Picker | Self::ScrollButton(..)
        ) || self.has_argument()
    }

    fn valid_after_slotted(&self) -> bool {
        matches!(
            self,
            Self::Before
                | Self::After
                | Self::Marker
                | Self::Placeholder
                | Self::FileSelectorButton
                | Self::Checkmark
                | Self::PickerIcon
                | Self::Picker
        )
    }

    // ::before and ::after accept ::marker, and so does ::column, which also
    // accepts ::scroll-marker.
    fn valid_after_before_or_after(&self) -> bool {
        matches!(self, Self::Marker | Self::Column | Self::ScrollMarker)
    }

    fn is_before_or_after(&self) -> bool {
        matches!(self, Self::Before | Self::After | Self::Column)
    }

    fn specificity_count(&self) -> u32 {
        match *self {
            Self::ViewTransitionGroup(argument)
            | Self::ViewTransitionImagePair(argument)
            | Self::ViewTransitionOld(argument)
            | Self::ViewTransitionNew(argument) => {
                // A named view transition pseudo-element selector with only the
                // universal name and no classes has no specificity.
                let names = PT_NAMES_AND_CLASSES.read().unwrap();
                let (name, classes) = &names[argument.0 as usize];
                u32::from(name.is_some() || !classes.is_empty())
            },
            _ => 1,
        }
    }

    fn is_in_pseudo_element_tree(&self) -> bool {
        self.has_argument()
    }

    fn accepts_state_pseudo_classes(&self) -> bool {
        // `:only-child` is allowed after a descendant of a pseudo-element root.
        self.is_in_pseudo_element_tree()
            || matches!(
                self,
                Self::DetailsContent
                    | Self::FileSelectorButton
                    | Self::ScrollButton(..)
                    | Self::Placeholder
                    | Self::ColorSwatch
                    | Self::SliderFill
                    | Self::SliderThumb
                    | Self::SliderTrack
                    | Self::MozProgressBar
                    | Self::UnknownWebkit(..)
            )
    }
}

/// The number of eager pseudo-elements. Keep this in sync with cascade_type.
pub const EAGER_PSEUDO_COUNT: usize = 5;

impl PseudoElement {
    /// Gets the canonical index of this eagerly-cascaded pseudo-element.
    #[inline]
    pub fn eager_index(&self) -> usize {
        debug_assert!(self.is_eager());
        self.index()
    }

    /// An index for this pseudo-element to be indexed in an enumerated array.
    #[inline]
    pub fn index(&self) -> usize {
        use self::PseudoElement::*;
        match *self {
            After => 0,
            Before => 1,
            Selection => 2,
            FirstLetter => 3,
            FirstLine => 4,
            Backdrop => 5,
            DetailsContent => 6,
            Marker => 7,
            ColorSwatch => 8,
            FileSelectorButton => 9,
            Placeholder => 10,
            SliderFill => 11,
            SliderThumb => 12,
            SliderTrack => 13,
            MozProgressBar => 14,
            ServoTextControlInnerContainer => 15,
            ServoTextControlInnerEditor => 16,
            ServoAnonymousBox => 17,
            ServoAnonymousTable => 18,
            ServoAnonymousTableCell => 19,
            ServoAnonymousTableRow => 20,
            ServoTableGrid => 21,
            ServoTableWrapper => 22,
            ViewTransition => 23,
            ViewTransitionGroup(..) | ViewTransitionImagePair(..) => 24,
            ViewTransitionOld(..) | ViewTransitionNew(..) => 25,
            TargetText => 26,
            SpellingError => 27,
            GrammarError => 28,
            SearchText => 29,
            Picker => 30,
            PickerIcon => 31,
            Checkmark => 32,
            ScrollMarker => 33,
            ScrollMarkerGroup => 34,
            ScrollButton(..) => 35,
            Column => 36,
            UnknownWebkit(..) => 37,
            Highlight(..) => 38,
            InterestButton => 39,
        }
    }

    /// An array of `None`, one per pseudo-element.
    pub fn pseudo_none_array<T>() -> [Option<T>; PSEUDO_COUNT] {
        std::array::from_fn(|_| None)
    }

    /// Creates a pseudo-element from an eager index.
    #[inline]
    pub fn from_eager_index(i: usize) -> Self {
        let result = match i {
            0 => PseudoElement::After,
            1 => PseudoElement::Before,
            2 => PseudoElement::Selection,
            3 => PseudoElement::FirstLetter,
            4 => PseudoElement::FirstLine,
            _ => panic!("{i} is not an eager pseudo-element index"),
        };
        debug_assert!(result.is_eager());
        result
    }

    /// Whether the current pseudo element is ::before or ::after.
    #[inline]
    pub fn is_before_or_after(&self) -> bool {
        self.is_before() || self.is_after()
    }

    /// Whether this is an unknown ::-webkit- pseudo-element.
    #[inline]
    pub fn is_unknown_webkit_pseudo_element(&self) -> bool {
        matches!(*self, PseudoElement::UnknownWebkit(..))
    }

    /// Whether this pseudo-element is the ::marker pseudo.
    #[inline]
    pub fn is_marker(&self) -> bool {
        *self == PseudoElement::Marker
    }

    /// Whether this pseudo-element is the ::selection pseudo.
    #[inline]
    pub fn is_selection(&self) -> bool {
        *self == PseudoElement::Selection
    }

    /// Whether this pseudo-element is the ::before pseudo.
    #[inline]
    pub fn is_before(&self) -> bool {
        *self == PseudoElement::Before
    }

    /// Whether this pseudo-element is the ::after pseudo.
    #[inline]
    pub fn is_after(&self) -> bool {
        *self == PseudoElement::After
    }

    /// Whether the current pseudo element is :first-letter
    #[inline]
    pub fn is_first_letter(&self) -> bool {
        *self == PseudoElement::FirstLetter
    }

    /// Whether the current pseudo element is :first-line
    #[inline]
    pub fn is_first_line(&self) -> bool {
        *self == PseudoElement::FirstLine
    }

    /// Whether this pseudo-element is representing the color swatch
    /// inside an `<input>` element.
    #[inline]
    pub fn is_color_swatch(&self) -> bool {
        *self == PseudoElement::ColorSwatch
    }

    /// Whether this pseudo-element is eagerly-cascaded.
    #[inline]
    pub fn is_eager(&self) -> bool {
        self.cascade_type() == PseudoElementCascadeType::Eager
    }

    /// Whether this pseudo-element is lazily-cascaded.
    #[inline]
    pub fn is_lazy(&self) -> bool {
        self.cascade_type() == PseudoElementCascadeType::Lazy
    }

    /// Whether this pseudo-element is for an anonymous box.
    pub fn is_anon_box(&self) -> bool {
        self.is_precomputed()
    }

    /// Whether this pseudo-element skips flex/grid container display-based
    /// fixup.
    #[inline]
    pub fn skip_item_display_fixup(&self) -> bool {
        !self.is_before_or_after()
    }

    /// Whether this pseudo-element is precomputed.
    #[inline]
    pub fn is_precomputed(&self) -> bool {
        self.cascade_type() == PseudoElementCascadeType::Precomputed
    }

    /// Returns which kind of cascade type has this pseudo.
    ///
    /// See the documentation for `PseudoElementCascadeType` for how we choose
    /// which cascade type to use.
    ///
    /// Note: Keep eager pseudos in sync with `EAGER_PSEUDO_COUNT` and
    /// `EMPTY_PSEUDO_ARRAY` in `style/data.rs`
    #[inline]
    pub fn cascade_type(&self) -> PseudoElementCascadeType {
        match *self {
            PseudoElement::After
            | PseudoElement::Before
            | PseudoElement::FirstLetter
            | PseudoElement::FirstLine
            | PseudoElement::Selection => PseudoElementCascadeType::Eager,
            PseudoElement::Backdrop
            | PseudoElement::ColorSwatch
            | PseudoElement::FileSelectorButton
            | PseudoElement::InterestButton
            | PseudoElement::Marker
            | PseudoElement::Placeholder
            | PseudoElement::DetailsContent
            | PseudoElement::SliderFill
            | PseudoElement::SliderThumb
            | PseudoElement::SliderTrack
            | PseudoElement::MozProgressBar
            | PseudoElement::ServoTextControlInnerContainer
            | PseudoElement::ServoTextControlInnerEditor
            | PseudoElement::ViewTransition
            | PseudoElement::ViewTransitionGroup(..)
            | PseudoElement::ViewTransitionImagePair(..)
            | PseudoElement::ViewTransitionOld(..)
            | PseudoElement::ViewTransitionNew(..)
            | PseudoElement::TargetText
            | PseudoElement::SpellingError
            | PseudoElement::GrammarError
            | PseudoElement::SearchText
            | PseudoElement::Picker
            | PseudoElement::PickerIcon
            | PseudoElement::Checkmark
            | PseudoElement::ScrollMarker
            | PseudoElement::ScrollMarkerGroup
            | PseudoElement::ScrollButton(..)
            | PseudoElement::Column
            | PseudoElement::UnknownWebkit(..)
            | PseudoElement::Highlight(..) => PseudoElementCascadeType::Lazy,
            PseudoElement::ServoAnonymousBox
            | PseudoElement::ServoAnonymousTable
            | PseudoElement::ServoAnonymousTableCell
            | PseudoElement::ServoAnonymousTableRow
            | PseudoElement::ServoTableGrid
            | PseudoElement::ServoTableWrapper => PseudoElementCascadeType::Precomputed,
        }
    }

    /// Covert non-canonical pseudo-element to canonical one, and keep a
    /// canonical one as it is.
    pub fn canonical(&self) -> PseudoElement {
        self.clone()
    }

    /// Stub, only Gecko needs this
    pub fn pseudo_info(&self) {
        ()
    }

    /// Property flag that properties must have to apply to this pseudo-element.
    #[inline]
    pub fn property_restriction(&self) -> PropertyFlags {
        match self {
            PseudoElement::FirstLetter => PropertyFlags::APPLIES_TO_FIRST_LETTER,
            PseudoElement::FirstLine => PropertyFlags::APPLIES_TO_FIRST_LINE,
            PseudoElement::Marker if crate::pref!("layout.css.marker.restricted") => {
                PropertyFlags::APPLIES_TO_MARKER
            },
            PseudoElement::Placeholder => PropertyFlags::APPLIES_TO_PLACEHOLDER,
            PseudoElement::Selection
            | PseudoElement::TargetText
            | PseudoElement::SpellingError
            | PseudoElement::GrammarError
            | PseudoElement::SearchText
            | PseudoElement::Highlight(..) => PropertyFlags::APPLIES_TO_HIGHLIGHT,
            _ => PropertyFlags::empty(),
        }
    }

    /// Whether this pseudo-element should actually exist if it has
    /// the given styles.
    pub fn should_exist(&self, style: &ComputedValues) -> bool {
        let display = *style.get_box().get_display();
        if display == Display::None {
            return false;
        }
        if self.is_before_or_after() && style.ineffective_content_property() {
            return false;
        }

        true
    }

    /// Whether this pseudo-element is the ::highlight pseudo.
    pub fn is_highlight(&self) -> bool {
        matches!(*self, Self::Highlight(..))
    }

    /// Whether this pseudo-element stands for a single pseudo-element of its
    /// originating element, which a view transition pseudo-element without a
    /// name in its argument does not.
    pub fn names_one_element(&self) -> bool {
        match *self {
            Self::ViewTransitionGroup(argument)
            | Self::ViewTransitionImagePair(argument)
            | Self::ViewTransitionOld(argument)
            | Self::ViewTransitionNew(argument) => PT_NAMES_AND_CLASSES.read().unwrap()
                [argument.0 as usize]
                .0
                .is_some(),
            _ => true,
        }
    }

    /// Whether this pseudo-element takes an argument.
    #[inline]
    pub fn has_argument(&self) -> bool {
        matches!(
            self,
            Self::ViewTransitionGroup(..)
                | Self::ViewTransitionImagePair(..)
                | Self::ViewTransitionOld(..)
                | Self::ViewTransitionNew(..)
        )
    }

    /// Whether this pseudo-element is the ::target-text pseudo.
    #[inline]
    pub fn is_target_text(&self) -> bool {
        *self == PseudoElement::TargetText
    }

    /// Whether this is a highlight pseudo-element that is styled lazily during
    /// painting rather than during the restyle traversal. These pseudos need
    /// explicit repaint triggering when their styles change.
    #[inline]
    pub fn is_lazy_painted_highlight_pseudo(&self) -> bool {
        self.is_selection() || self.is_highlight() || self.is_target_text()
    }

    /// Whether this pseudo-element is "element-backed", which means that it inherits from its regular
    /// flat tree parent, which might not be the originating element.
    #[inline]
    pub fn is_element_backed(&self) -> bool {
        use selectors::parser::PseudoElement;
        self.parses_as_element_backed()
            || matches!(
                self,
                Self::Placeholder
                    | Self::ColorSwatch
                    | Self::FileSelectorButton
                    | Self::SliderFill
                    | Self::SliderThumb
                    | Self::SliderTrack
                    | Self::MozProgressBar
                    | Self::ServoTextControlInnerContainer
                    | Self::ServoTextControlInnerEditor,
            )
    }
}

/// The type used for storing `:lang` arguments.
pub type Lang = Box<str>;

/// The type used to store the state argument to the `:state` pseudo-class.
#[derive(Clone, Debug, Eq, Hash, MallocSizeOf, PartialEq, ToCss, ToShmem)]
pub struct CustomState(pub AtomIdent);

/// The heading levels listed in a `:heading()` pseudo-class.
/// https://drafts.csswg.org/selectors-5/#headings
#[derive(Clone, Debug, Eq, Hash, MallocSizeOf, PartialEq, ToShmem)]
pub struct HeadingSelectorData(pub thin_vec::ThinVec<CSSInteger>);

impl HeadingSelectorData {
    /// Whether the heading level packed into `state` is one of the listed
    /// levels. An empty list, from a bare `:heading`, matches any level.
    pub fn matches_state(&self, state: ElementState) -> bool {
        let bits = state.intersection(ElementState::HEADING_LEVEL_BITS).bits();
        if bits == 0 {
            return false;
        }
        if self.0.is_empty() {
            return true;
        }
        let level = (bits >> dom::HEADING_LEVEL_OFFSET) as CSSInteger;
        self.0.contains(&level)
    }
}

/// A non tree-structural pseudo-class.
/// See https://drafts.csswg.org/selectors-4/#structural-pseudos
#[derive(Clone, Debug, Eq, Hash, MallocSizeOf, PartialEq, ToShmem)]
#[allow(missing_docs)]
pub enum NonTSPseudoClass {
    Active,
    AnyLink,
    Autofill,
    Buffering,
    Checked,
    /// The :state` pseudo-class.
    CustomState(CustomState),
    Default,
    Defined,
    /// The `:dir()` pseudo-class.
    Dir(Direction),
    Disabled,
    Enabled,
    Focus,
    FocusWithin,
    FocusVisible,
    Fullscreen,
    HasSlotted,
    /// The `:heading` and `:heading()` pseudo-classes.
    Heading(HeadingSelectorData),
    Hover,
    InRange,
    Indeterminate,
    InterestSource,
    InterestTarget,
    Invalid,
    Lang(Lang),
    Link,
    Modal,
    MozInert,
    MozMeterOptimum,
    MozMeterSubOptimum,
    MozMeterSubSubOptimum,
    Muted,
    Open,
    Optional,
    OutOfRange,
    Paused,
    PlaceholderShown,
    Playing,
    PopoverOpen,
    ReadOnly,
    ReadWrite,
    Required,
    Seeking,
    ServoNonZeroBorder,
    Stalled,
    Target,
    UserInvalid,
    UserValid,
    Valid,
    Visited,
    VolumeLocked,
}

impl ::selectors::parser::NonTSPseudoClass for NonTSPseudoClass {
    #[inline]
    fn is_active_or_hover(&self) -> bool {
        matches!(*self, NonTSPseudoClass::Active | NonTSPseudoClass::Hover)
    }

    #[inline]
    fn is_user_action_state(&self) -> bool {
        matches!(
            *self,
            NonTSPseudoClass::Active | NonTSPseudoClass::Hover | NonTSPseudoClass::Focus
        )
    }

    fn visit<V>(&self, _: &mut V) -> bool {
        true
    }
}

impl ToCss for NonTSPseudoClass {
    fn to_css<W>(&self, dest: &mut W) -> fmt::Result
    where
        W: fmt::Write,
    {
        use self::NonTSPseudoClass::*;
        if let Lang(ref lang) = *self {
            dest.write_str(":lang(")?;
            serialize_identifier(lang, dest)?;
            return dest.write_char(')');
        }

        dest.write_str(match *self {
            Self::Active => ":active",
            Self::AnyLink => ":any-link",
            Self::Autofill => ":autofill",
            Self::Buffering => ":buffering",
            Self::Checked => ":checked",
            Self::CustomState(ref state) => {
                dest.write_str(":state(")?;
                state.0.to_css(dest)?;
                return dest.write_char(')');
            },
            Self::Default => ":default",
            Self::Defined => ":defined",
            Self::Dir(ref dir) => {
                dest.write_str(":dir(")?;
                style_traits::ToCss::to_css(dir, &mut style_traits::CssWriter::new(dest))?;
                return dest.write_char(')');
            },
            Self::Disabled => ":disabled",
            Self::Enabled => ":enabled",
            Self::Focus => ":focus",
            Self::FocusVisible => ":focus-visible",
            Self::FocusWithin => ":focus-within",
            Self::Fullscreen => ":fullscreen",
            Self::HasSlotted => ":has-slotted",
            Self::Heading(ref levels) => {
                dest.write_str(":heading")?;
                if levels.0.is_empty() {
                    return Ok(());
                }
                dest.write_char('(')?;
                for (index, level) in levels.0.iter().enumerate() {
                    if index > 0 {
                        dest.write_str(", ")?;
                    }
                    write!(dest, "{level}")?;
                }
                return dest.write_char(')');
            },
            Self::Hover => ":hover",
            Self::InRange => ":in-range",
            Self::Indeterminate => ":indeterminate",
            Self::InterestSource => ":interest-source",
            Self::InterestTarget => ":interest-target",
            Self::Invalid => ":invalid",
            Self::Link => ":link",
            Self::Modal => ":modal",
            Self::MozInert => ":-moz-inert",
            Self::MozMeterOptimum => ":-moz-meter-optimum",
            Self::MozMeterSubOptimum => ":-moz-meter-sub-optimum",
            Self::MozMeterSubSubOptimum => ":-moz-meter-sub-sub-optimum",
            Self::Muted => ":muted",
            Self::Open => ":open",
            Self::Optional => ":optional",
            Self::OutOfRange => ":out-of-range",
            Self::Paused => ":paused",
            Self::PlaceholderShown => ":placeholder-shown",
            Self::Playing => ":playing",
            Self::PopoverOpen => ":popover-open",
            Self::ReadOnly => ":read-only",
            Self::ReadWrite => ":read-write",
            Self::Required => ":required",
            Self::Seeking => ":seeking",
            Self::ServoNonZeroBorder => ":-servo-nonzero-border",
            Self::Stalled => ":stalled",
            Self::Target => ":target",
            Self::UserInvalid => ":user-invalid",
            Self::UserValid => ":user-valid",
            Self::Valid => ":valid",
            Self::Visited => ":visited",
            Self::VolumeLocked => ":volume-locked",
            Self::Lang(_) => unreachable!(),
        })
    }
}

impl NonTSPseudoClass {
    /// Gets a given state flag for this pseudo-class. This is used to do
    /// selector matching, and it's set from the DOM.
    pub fn state_flag(&self) -> ElementState {
        match *self {
            Self::Active => ElementState::ACTIVE,
            Self::AnyLink => ElementState::VISITED_OR_UNVISITED,
            Self::Autofill => ElementState::AUTOFILL,
            Self::Buffering => ElementState::BUFFERING,
            Self::Checked => ElementState::CHECKED,
            Self::Default => ElementState::DEFAULT,
            Self::Defined => ElementState::DEFINED,
            Self::Dir(ref dir) => dir.element_state(),
            Self::Disabled => ElementState::DISABLED,
            Self::Enabled => ElementState::ENABLED,
            Self::Focus => ElementState::FOCUS,
            Self::FocusVisible => ElementState::FOCUSRING,
            Self::FocusWithin => ElementState::FOCUS_WITHIN,
            Self::Fullscreen => ElementState::FULLSCREEN,
            Self::HasSlotted => ElementState::HAS_SLOTTED,
            Self::Heading(_) => ElementState::HEADING_LEVEL_BITS,
            Self::Hover => ElementState::HOVER,
            Self::InRange => ElementState::INRANGE,
            Self::Indeterminate => ElementState::INDETERMINATE,
            Self::InterestSource => ElementState::INTEREST_SOURCE,
            Self::InterestTarget => ElementState::INTEREST_TARGET,
            Self::Invalid => ElementState::INVALID,
            Self::Link => ElementState::UNVISITED,
            Self::Modal => ElementState::MODAL,
            Self::MozInert => ElementState::INERT,
            Self::MozMeterOptimum => ElementState::OPTIMUM,
            Self::MozMeterSubOptimum => ElementState::SUB_OPTIMUM,
            Self::MozMeterSubSubOptimum => ElementState::SUB_SUB_OPTIMUM,
            Self::Muted => ElementState::MUTED,
            Self::Open => ElementState::OPEN,
            Self::Optional => ElementState::OPTIONAL_,
            Self::OutOfRange => ElementState::OUTOFRANGE,
            Self::Paused | Self::Playing => ElementState::PAUSED,
            Self::PlaceholderShown => ElementState::PLACEHOLDER_SHOWN,
            Self::PopoverOpen => ElementState::POPOVER_OPEN,
            Self::ReadOnly => ElementState::READONLY,
            Self::ReadWrite => ElementState::READWRITE,
            Self::Required => ElementState::REQUIRED,
            Self::Seeking => ElementState::SEEKING,
            Self::Stalled => ElementState::STALLED,
            Self::Target => ElementState::URLTARGET,
            Self::UserInvalid => ElementState::USER_INVALID,
            Self::UserValid => ElementState::USER_VALID,
            Self::Valid => ElementState::VALID,
            Self::Visited => ElementState::VISITED,
            Self::CustomState(_)
            | Self::Lang(_)
            | Self::ServoNonZeroBorder
            | Self::VolumeLocked => ElementState::empty(),
        }
    }

    /// Get the document state flag associated with a pseudo-class, if any.
    pub fn document_state_flag(&self) -> DocumentState {
        DocumentState::empty()
    }

    /// Returns true if the given pseudoclass should trigger style sharing cache revalidation.
    pub fn needs_cache_revalidation(&self) -> bool {
        self.state_flag().is_empty()
    }
}

/// The abstract struct we implement the selector parser implementation on top
/// of.
#[derive(Clone, Debug, MallocSizeOf, PartialEq)]
pub struct SelectorImpl;

/// A set of extra data to carry along with the matching context, either for
/// selector-matching or invalidation.
#[derive(Debug, Default)]
pub struct ExtraMatchingData<'a> {
    /// The invalidation data to invalidate doc-state pseudo-classes correctly.
    pub invalidation_data: InvalidationMatchingData,

    /// The invalidation bits from matching container queries. These are here
    /// just for convenience mostly.
    pub cascade_input_flags: ComputedValueFlags,

    /// The style of the originating element in order to evaluate @container
    /// size queries affecting pseudo-elements.
    pub originating_element_style: Option<&'a ComputedValues>,
}

impl ::selectors::SelectorImpl for SelectorImpl {
    type PseudoElement = PseudoElement;
    type NonTSPseudoClass = NonTSPseudoClass;

    type ExtraMatchingData<'a> = ExtraMatchingData<'a>;
    type AttrValue = AtomString;
    type Identifier = AtomIdent;
    type LocalName = LocalName;
    type NamespacePrefix = Prefix;
    type NamespaceUrl = Namespace;
    type BorrowedLocalName = web_atoms::LocalName;
    type BorrowedNamespaceUrl = web_atoms::Namespace;
}

impl<'a, 'i> ::selectors::Parser<'i> for SelectorParser<'a> {
    type Impl = SelectorImpl;
    type Error = StyleParseErrorKind;

    #[inline]
    fn parse_nth_child_of(&self) -> bool {
        crate::pref!("layout.css.nth-child-of.enabled")
    }

    #[inline]
    fn parse_is_and_where(&self) -> bool {
        true
    }

    #[inline]
    fn parse_has(&self) -> bool {
        crate::pref!("layout.css.has-selector.enabled")
    }

    #[inline]
    fn parse_class_prefix(&self) -> bool {
        crate::pref!("layout.css.class-prefix-selector.enabled")
    }

    #[inline]
    fn parse_has_slotted_argument(&self) -> bool {
        crate::pref!("layout.css.has-slotted-argument.enabled")
    }

    #[inline]
    fn parse_host_context(&self) -> bool {
        crate::pref!("layout.css.host-context.enabled")
    }

    #[inline]
    fn parse_parent_selector(&self) -> bool {
        true
    }

    #[inline]
    fn parse_part(&self) -> bool {
        true
    }

    #[inline]
    fn allow_forgiving_selectors(&self) -> bool {
        !self.for_supports_rule
    }

    fn parse_non_ts_pseudo_class(&self, name: &str) -> Result<NonTSPseudoClass, ParseError> {
        let pseudo_class = match_ignore_ascii_case! { &name,
            "active" => NonTSPseudoClass::Active,
            "any-link" => NonTSPseudoClass::AnyLink,
            "autofill" => NonTSPseudoClass::Autofill,
            "buffering" => NonTSPseudoClass::Buffering,
            "checked" => NonTSPseudoClass::Checked,
            "default" => NonTSPseudoClass::Default,
            "defined" => NonTSPseudoClass::Defined,
            "disabled" => NonTSPseudoClass::Disabled,
            "enabled" => NonTSPseudoClass::Enabled,
            "focus" => NonTSPseudoClass::Focus,
            "focus-visible" => NonTSPseudoClass::FocusVisible,
            "focus-within" => NonTSPseudoClass::FocusWithin,
            "fullscreen" => NonTSPseudoClass::Fullscreen,
            "has-slotted" => NonTSPseudoClass::HasSlotted,
            "heading" => NonTSPseudoClass::Heading(HeadingSelectorData(Default::default())),
            "hover" => NonTSPseudoClass::Hover,
            "in-range" => NonTSPseudoClass::InRange,
            "indeterminate" => NonTSPseudoClass::Indeterminate,
            "interest-source" if crate::pref!("layout.css.interest-invokers.enabled") => NonTSPseudoClass::InterestSource,
            "interest-target" if crate::pref!("layout.css.interest-invokers.enabled") => NonTSPseudoClass::InterestTarget,
            "invalid" => NonTSPseudoClass::Invalid,
            "link" => NonTSPseudoClass::Link,
            "modal" => NonTSPseudoClass::Modal,
            "muted" => NonTSPseudoClass::Muted,
            "open" => NonTSPseudoClass::Open,
            "optional" => NonTSPseudoClass::Optional,
            "out-of-range" => NonTSPseudoClass::OutOfRange,
            "paused" => NonTSPseudoClass::Paused,
            "placeholder-shown" => NonTSPseudoClass::PlaceholderShown,
            "playing" => NonTSPseudoClass::Playing,
            "popover-open" => NonTSPseudoClass::PopoverOpen,
            "read-only" => NonTSPseudoClass::ReadOnly,
            "read-write" => NonTSPseudoClass::ReadWrite,
            "required" => NonTSPseudoClass::Required,
            "seeking" => NonTSPseudoClass::Seeking,
            "stalled" => NonTSPseudoClass::Stalled,
            "target" => NonTSPseudoClass::Target,
            "user-invalid" => NonTSPseudoClass::UserInvalid,
            "user-valid" => NonTSPseudoClass::UserValid,
            "valid" => NonTSPseudoClass::Valid,
            "visited" => NonTSPseudoClass::Visited,
            "volume-locked" => NonTSPseudoClass::VolumeLocked,
            "-moz-inert" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(
                        SelectorParseErrorKind::UnexpectedIdent
                    ))
                }
                NonTSPseudoClass::MozInert
            },
            "-moz-meter-optimum" => NonTSPseudoClass::MozMeterOptimum,
            "-moz-meter-sub-optimum" => NonTSPseudoClass::MozMeterSubOptimum,
            "-moz-meter-sub-sub-optimum" => NonTSPseudoClass::MozMeterSubSubOptimum,
            "-servo-nonzero-border" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(
                        SelectorParseErrorKind::UnexpectedIdent
                    ))
                }
                NonTSPseudoClass::ServoNonZeroBorder
            },
            _ => return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent)),
        };

        Ok(pseudo_class)
    }

    fn parse_non_ts_functional_pseudo_class(
        &self,
        name: &str,
        parser: &mut CssParser<'i>,
        _after_part: bool,
    ) -> Result<NonTSPseudoClass, ParseError> {
        let pseudo_class = match_ignore_ascii_case! { &name,
            "lang" => {
                NonTSPseudoClass::Lang(parser.expect_ident_or_string()?.as_ref().into())
            },
            "state" => {
                let result = AtomIdent::from(parser.expect_ident()?.as_ref());
                NonTSPseudoClass::CustomState(CustomState(result))
            },
            "dir" => NonTSPseudoClass::Dir(Direction::parse(parser)?),
            "heading" => {
                let levels = parser.parse_comma_separated(|input| Ok(input.expect_integer()?))?;
                NonTSPseudoClass::Heading(HeadingSelectorData(levels.into()))
            },
            _ => return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent)),
        };

        Ok(pseudo_class)
    }

    fn parse_pseudo_element(&self, name: &str) -> Result<PseudoElement, ParseError> {
        use self::PseudoElement::*;
        let pseudo_element = match_ignore_ascii_case! { &name,
            "before" => Before,
            "after" => After,
            "backdrop" => Backdrop,
            "selection" => Selection,
            "file-selector-button" => FileSelectorButton,
            "first-letter" => FirstLetter,
            "first-line" if crate::pref!("layout.css.first-line.enabled") => FirstLine,
            "marker" => Marker,
            "details-content" => DetailsContent,
            "interest-button" if crate::pref!("layout.css.interest-invokers.enabled") => InterestButton,
            "color-swatch" => ColorSwatch,
            "placeholder" => Placeholder,
            "-servo-text-control-inner-container" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent))
                }
                ServoTextControlInnerContainer
            },
            "-servo-text-control-inner-editor" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent))
                }
                ServoTextControlInnerEditor
            },
            "view-transition" => ViewTransition,
            "target-text" => TargetText,
            "spelling-error" => SpellingError,
            "grammar-error" => GrammarError,
            "search-text" => SearchText,
            "picker-icon" => PickerIcon,
            "checkmark" => Checkmark,
            "scroll-marker" => ScrollMarker,
            "scroll-marker-group" => ScrollMarkerGroup,
            "column" => Column,
            "slider-fill" => SliderFill,
            "slider-thumb" => SliderThumb,
            "slider-track" => SliderTrack,
            "-moz-progress-bar" => MozProgressBar,
            "-servo-anonymous-box" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent))
                }
                ServoAnonymousBox
            },
            "-servo-anonymous-table" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent))
                }
                ServoAnonymousTable
            },
            "-servo-anonymous-table-row" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent))
                }
                ServoAnonymousTableRow
            },
            "-servo-anonymous-table-cell" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent))
                }
                ServoAnonymousTableCell
            },
            "-servo-table-grid" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent))
                }
                ServoTableGrid
            },
            "-servo-table-wrapper" => {
                if !self.in_user_agent_stylesheet() {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent))
                }
                ServoTableWrapper
            },
            _ => {
                // @supports must report unknown `-webkit-*` pseudo-elements as unsupported.
                if self.for_supports_rule
                    || name.len() <= 8
                    || !name.as_bytes()[..8].eq_ignore_ascii_case(b"-webkit-")
                {
                    return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent));
                }
                UnknownWebkit(UnknownWebkitName::intern(name[8..].to_ascii_lowercase().into()))
            }
        };

        Ok(pseudo_element)
    }

    fn parse_functional_pseudo_element(
        &self,
        name: &str,
        arguments: &mut CssParser<'i>,
    ) -> Result<PseudoElement, ParseError> {
        use self::PseudoElement::*;
        if name.eq_ignore_ascii_case("picker") {
            if !arguments.expect_ident()?.eq_ignore_ascii_case("select") {
                return Err(ParseError::custom(SelectorParseErrorKind::UnexpectedIdent));
            }
            return Ok(Picker);
        }
        if name.eq_ignore_ascii_case("scroll-button") {
            return Ok(ScrollButton(ScrollButtonDirection::parse(arguments)?));
        }
        if name.eq_ignore_ascii_case("highlight") {
            let name = arguments.expect_ident()?;
            return Ok(Highlight(HighlightName::intern(Atom::from(&**name))));
        }
        let make: fn(PtNameAndClassSelector) -> PseudoElement = match_ignore_ascii_case! { name,
            "view-transition-group" => ViewTransitionGroup,
            "view-transition-image-pair" => ViewTransitionImagePair,
            "view-transition-old" => ViewTransitionOld,
            "view-transition-new" => ViewTransitionNew,
            _ => return Err(ParseError::custom(
                SelectorParseErrorKind::UnsupportedPseudoClassOrElement,
            )),
        };
        Ok(make(PtNameAndClassSelector::parse(arguments)?))
    }

    fn default_namespace(&self) -> Option<Namespace> {
        self.namespaces.default.as_ref().map(|ns| ns.clone())
    }

    fn namespace_for_prefix(&self, prefix: &Prefix) -> Option<Namespace> {
        self.namespaces.prefixes.get(prefix).cloned()
    }

    fn parse_host(&self) -> bool {
        true
    }

    fn parse_slotted(&self) -> bool {
        true
    }
}

impl SelectorImpl {
    /// A helper to traverse each eagerly cascaded pseudo-element, executing
    /// `fun` on it.
    #[inline]
    pub fn each_eagerly_cascaded_pseudo_element<F>(mut fun: F)
    where
        F: FnMut(PseudoElement),
    {
        for i in 0..EAGER_PSEUDO_COUNT {
            fun(PseudoElement::from_eager_index(i));
        }
    }
}

/// A map from elements to snapshots for the Servo style back-end.
#[derive(Debug)]
pub struct SnapshotMap(FxHashMap<OpaqueNode, ServoElementSnapshot>);

impl SnapshotMap {
    /// Create a new empty `SnapshotMap`.
    pub fn new() -> Self {
        SnapshotMap(FxHashMap::default())
    }

    /// Get a snapshot given an element.
    pub fn get<T: TElement>(&self, el: &T) -> Option<&ServoElementSnapshot> {
        self.0.get(&el.as_node().opaque())
    }
}

impl Deref for SnapshotMap {
    type Target = FxHashMap<OpaqueNode, ServoElementSnapshot>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for SnapshotMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Servo's version of an element snapshot.
#[derive(Debug, Default, MallocSizeOf)]
pub struct ServoElementSnapshot {
    /// The stored state of the element.
    pub state: Option<ElementState>,
    /// The set of stored attributes and its values.
    pub attrs: Option<Vec<(AttrIdentifier, AttrValue)>>,
    /// The set of changed attributes and its values.
    pub changed_attrs: Vec<LocalName>,
    /// Whether the class attribute changed or not.
    pub class_changed: bool,
    /// Whether the id attribute changed or not.
    pub id_changed: bool,
    /// Whether other attributes other than id or class changed or not.
    pub other_attributes_changed: bool,
}

impl ServoElementSnapshot {
    /// Create an empty element snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the id attribute changed or not.
    pub fn id_changed(&self) -> bool {
        self.id_changed
    }

    /// Returns whether the class attribute changed or not.
    pub fn class_changed(&self) -> bool {
        self.class_changed
    }

    /// Returns whether other attributes other than id or class changed or not.
    pub fn other_attr_changed(&self) -> bool {
        self.other_attributes_changed
    }

    fn get_attr(&self, namespace: &Namespace, name: &LocalName) -> Option<&AttrValue> {
        self.attrs
            .as_ref()?
            .iter()
            .find(|&&(ref ident, _)| ident.local_name == *name && ident.namespace == *namespace)
            .map(|&(_, ref v)| v)
    }

    /// Executes the callback once for each attribute that changed.
    #[inline]
    pub fn each_attr_changed<F>(&self, mut callback: F)
    where
        F: FnMut(&LocalName),
    {
        for name in &self.changed_attrs {
            callback(name)
        }
    }

    fn any_attr_ignore_ns<F>(&self, name: &LocalName, mut f: F) -> bool
    where
        F: FnMut(&AttrValue) -> bool,
    {
        self.attrs.as_ref().is_some_and(|attrs| {
            attrs
                .iter()
                .any(|&(ref ident, ref v)| ident.local_name == *name && f(v))
        })
    }
}

impl ElementSnapshot for ServoElementSnapshot {
    fn state(&self) -> Option<ElementState> {
        self.state.clone()
    }

    fn has_attrs(&self) -> bool {
        self.attrs.is_some()
    }

    fn id_attr(&self) -> Option<&Atom> {
        self.get_attr(&ns!(), &local_name!("id"))
            .map(|v| v.as_atom())
    }

    fn is_part(&self, part_name: &AtomIdent) -> bool {
        self.get_attr(&ns!(), &local_name!("part"))
            .is_some_and(|v| {
                v.as_tokens()
                    .iter()
                    .any(|atom| CaseSensitivity::CaseSensitive.eq_atom(atom, part_name))
            })
    }

    fn imported_part(&self, _: &AtomIdent) -> Option<AtomIdent> {
        None
    }

    fn has_class(&self, name: &AtomIdent, case_sensitivity: CaseSensitivity) -> bool {
        self.get_attr(&ns!(), &local_name!("class"))
            .map_or(false, |v| {
                v.as_tokens()
                    .iter()
                    .any(|atom| case_sensitivity.eq_atom(atom, name))
            })
    }

    fn each_class<F>(&self, mut callback: F)
    where
        F: FnMut(&AtomIdent),
    {
        if let Some(v) = self.get_attr(&ns!(), &local_name!("class")) {
            for class in v.as_tokens() {
                callback(AtomIdent::cast(class));
            }
        }
    }

    fn lang_attr(&self) -> Option<SelectorAttrValue> {
        self.get_attr(&ns!(xml), &local_name!("lang"))
            .or_else(|| self.get_attr(&ns!(), &local_name!("lang")))
            .map(|v| SelectorAttrValue::from(v as &str))
    }

    /// Returns true if the snapshot has stored state for custom states
    #[inline]
    fn has_custom_states(&self) -> bool {
        false
    }

    /// Returns true if the snapshot has a given CustomState
    #[inline]
    fn has_custom_state(&self, _state: &AtomIdent) -> bool {
        false
    }

    #[inline]
    fn each_custom_state<F>(&self, mut _callback: F)
    where
        F: FnMut(&AtomIdent),
    {
    }
}

impl ServoElementSnapshot {
    /// selectors::Element::attr_matches
    pub fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&Namespace>,
        local_name: &LocalName,
        operation: &AttrSelectorOperation<&AtomString>,
    ) -> bool {
        match *ns {
            NamespaceConstraint::Specific(ref ns) => self
                .get_attr(ns, local_name)
                .map_or(false, |value| value.eval_selector(operation)),
            NamespaceConstraint::Any => {
                self.any_attr_ignore_ns(local_name, |value| value.eval_selector(operation))
            },
        }
    }
}

/// Returns whether the language is matched, as defined by
/// [RFC 4647](https://tools.ietf.org/html/rfc4647#section-3.3.2).
pub fn extended_filtering(tag: &str, range: &str) -> bool {
    // A language tag subtag has at most eight characters, so a longer one makes the tag invalid.
    if tag.split('\x2d').any(|subtag| subtag.len() > 8) {
        return false;
    }
    range.split(',').any(|lang_range| {
        // step 1
        let mut range_subtags = lang_range.split('\x2d');
        let mut tag_subtags = tag.split('\x2d');

        // step 2
        // Note: [Level-4 spec](https://drafts.csswg.org/selectors/#lang-pseudo) check for wild card
        if let (Some(range_subtag), Some(tag_subtag)) = (range_subtags.next(), tag_subtags.next()) {
            if !(range_subtag.eq_ignore_ascii_case(tag_subtag)
                || range_subtag.eq_ignore_ascii_case("*"))
            {
                return false;
            }
        }

        let mut current_tag_subtag = tag_subtags.next();

        // step 3
        for range_subtag in range_subtags {
            // step 3a
            if range_subtag == "*" {
                continue;
            }
            match current_tag_subtag.clone() {
                Some(tag_subtag) => {
                    // step 3c
                    if range_subtag.eq_ignore_ascii_case(tag_subtag) {
                        current_tag_subtag = tag_subtags.next();
                        continue;
                    }
                    // step 3d
                    if tag_subtag.len() == 1 {
                        return false;
                    }
                    // else step 3e - continue with loop
                    current_tag_subtag = tag_subtags.next();
                    if current_tag_subtag.is_none() {
                        return false;
                    }
                },
                // step 3b
                None => {
                    return false;
                },
            }
        }
        // step 4
        true
    })
}
