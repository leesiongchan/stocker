use crate::stock::Stock;
use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use im::{ordmap, ordmap::OrdMap};
use math::round;
use parking_lot::{RwLock, RwLockWriteGuard};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{self, AtomicU16};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
use thiserror::Error;
use tui::{
    layout::{Margin, Rect},
    widgets::ListState,
};
use yahoo_finance::Interval;

pub struct App {
    pub stock: Stock,
    pub ui_state: UiState,
}

impl App {
    pub async fn load_stock(&mut self, symbol: &str) -> anyhow::Result<()> {
        self.stock.symbol = symbol.to_ascii_uppercase();

        self.ui_state.clear_date_range()?;

        self.stock.load_profile().await?;
        self.stock
            .load_historical_prices(
                self.ui_state.time_frame,
                self.ui_state.start_date,
                self.ui_state.end_date,
            )
            .await?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct UiState {
    debug_draw: bool,
    pub end_date: Option<DateTime<Utc>>,
    pub frame_rate_counter: FrameRateCounter,
    pub start_date: Option<DateTime<Utc>>,
    pub stock_symbol_input_state: InputState,
    target_areas: RwLock<OrdMap<UiTarget, Rect>>,
    pub time_frame: TimeFrame,
    pub time_frame_menu_state: MenuState<TimeFrame>,
}

impl UiState {
    pub fn debug_draw(&self) -> bool {
        self.debug_draw
    }

    pub fn set_debug_draw(&mut self, debug_draw: bool) -> anyhow::Result<()> {
        self.debug_draw = debug_draw;

        Ok(())
    }

    pub fn shift_date_range_before(&mut self, dt: DateTime<Utc>) -> anyhow::Result<()> {
        let time_frame_duration = self
            .time_frame
            .duration()
            .expect("time frame has no duration");

        let end_date = (dt - Duration::days(1)).date().and_hms(23, 59, 59);
        let start_date = (end_date - time_frame_duration + Duration::days(1))
            .date()
            .and_hms(0, 0, 0);

        self.start_date = Some(start_date);
        self.end_date = Some(end_date);

        Ok(())
    }

    pub fn shift_date_range_after(&mut self, dt: DateTime<Utc>) -> anyhow::Result<()> {
        let time_frame_duration = self
            .time_frame
            .duration()
            .expect("time frame has no duration");

        let start_date = (dt + Duration::days(1)).date().and_hms(0, 0, 0);
        let end_date = (start_date + time_frame_duration - Duration::days(1))
            .date()
            .and_hms(23, 59, 59);

        self.start_date = Some(start_date);
        self.end_date = Some(end_date);

        if end_date > Utc::now() {
            self.clear_date_range()?;
        }

        Ok(())
    }

    pub fn clear_date_range(&mut self) -> anyhow::Result<()> {
        self.start_date = None;
        self.end_date = None;

        Ok(())
    }

    pub fn input_cursor(
        &self,
        input_state: &InputState,
        input_target: UiTarget,
    ) -> Option<(u16, u16)> {
        let target_areas = self.target_areas.read();
        let input_area = target_areas.get(&input_target)?;
        let border_margin = Margin {
            horizontal: 1,
            vertical: 1,
        };
        let inner_area = input_area.inner(&border_margin);

        let cx = inner_area.left() + input_state.value.chars().count() as u16;
        let cy = inner_area.top();

        Some((cx, cy))
    }

    pub fn menu_index<T>(
        &self,
        menu_state: &MenuState<T>,
        menu_area: Rect,
        x: u16,
        y: u16,
    ) -> Option<usize>
    where
        T: Clone + PartialEq + ToString,
    {
        let border_margin = Margin {
            horizontal: 1,
            vertical: 1,
        };
        let inner_area = menu_area.inner(&border_margin);

        if inner_area.left() <= x
            && inner_area.right() >= x
            && inner_area.top() <= y
            && inner_area.bottom() >= y
        {
            if (inner_area.height as usize) < menu_state.items.len() {
                todo!("not sure how to select an item from scrollable list");
            }
            let n: usize = (y - inner_area.top()) as usize;

            if n < menu_state.items.len() {
                return Some(n);
            }
        }

        None
    }

    pub fn target_area(&self, x: u16, y: u16) -> Option<(UiTarget, Rect)> {
        self.target_areas
            .read()
            .clone()
            .into_iter()
            .rev()
            .find(|(_, area)| {
                area.left() <= x && area.right() >= x && area.top() <= y && area.bottom() >= y
            })
    }

    pub fn set_target_area(&self, target: UiTarget, area: Rect) -> anyhow::Result<()> {
        self.target_areas.write().insert(target, area);

        Ok(())
    }

    pub fn clear_target_areas(&self) -> anyhow::Result<()> {
        self.target_areas.write().clear();

        Ok(())
    }

    pub fn set_time_frame(&mut self, time_frame: TimeFrame) -> anyhow::Result<()> {
        self.time_frame = time_frame;
        self.time_frame_menu_state.select(time_frame)?;

        self.clear_date_range()?;

        Ok(())
    }
}

impl Default for UiState {
    fn default() -> Self {
        const DEFAULT_TIME_FRAME: TimeFrame = TimeFrame::OneMonth;

        Self {
            debug_draw: false,
            end_date: None,
            frame_rate_counter: FrameRateCounter::new(Duration::milliseconds(1_000)),
            start_date: None,
            stock_symbol_input_state: InputState::default(),
            target_areas: RwLock::new(ordmap! {}),
            time_frame: DEFAULT_TIME_FRAME,
            time_frame_menu_state: {
                let menu_state = MenuState::new(TimeFrame::iter());
                menu_state.select(DEFAULT_TIME_FRAME).unwrap();
                menu_state
            },
        }
    }
}

#[derive(Debug)]
pub struct MenuState<T>
where
    T: Clone + PartialEq + ToString,
{
    pub active: bool,
    pub items: Vec<T>,
    list_state: RwLock<ListState>,
}

impl<T> MenuState<T>
where
    T: Clone + PartialEq + ToString,
{
    pub fn new<I>(items: I) -> MenuState<T>
    where
        I: IntoIterator<Item = T>,
    {
        Self {
            active: false,
            items: items.into_iter().collect(),
            list_state: RwLock::new(ListState::default()),
        }
    }

    pub fn selected(&self) -> Option<T> {
        let selected = self.list_state.read().selected()?;

        Some(self.items[selected].clone())
    }

    pub fn select(&self, item: T) -> anyhow::Result<()> {
        let n = self
            .items
            .iter()
            .cloned()
            .position(|t| t == item)
            .with_context(|| "item not found")?;

        self.select_nth(n)?;

        Ok(())
    }

    pub fn select_prev(&self) -> anyhow::Result<()> {
        let selected = self
            .list_state
            .read()
            .selected()
            .with_context(|| "cannot select previous item when nothing is selected")?;

        if selected > 0 {
            self.select_nth(selected - 1)?;
        }

        Ok(())
    }

    pub fn select_next(&self) -> anyhow::Result<()> {
        let selected = self
            .list_state
            .read()
            .selected()
            .with_context(|| "cannot select next item when nothing is selected")?;

        if selected < self.items.len() - 1 {
            self.select_nth(selected + 1)?;
        }

        Ok(())
    }

    pub fn select_nth(&self, n: usize) -> anyhow::Result<()> {
        self.list_state.write().select(Some(n));

        Ok(())
    }

    pub fn clear_selection(&self) -> anyhow::Result<()> {
        self.list_state.write().select(None);

        Ok(())
    }

    pub fn list_state_write(&self) -> RwLockWriteGuard<ListState> {
        self.list_state.write()
    }
}

#[derive(Debug)]
pub struct InputState {
    pub active: bool,
    pub value: String,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            active: false,
            value: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UiTarget {
    StockName,
    StockSymbol,
    StockSymbolInput,
    TimeFrame,
    TimeFrameMenu,
}

impl UiTarget {
    pub fn zindex(self) -> i8 {
        match self {
            Self::StockName => 0,
            Self::StockSymbol => 0,
            Self::StockSymbolInput => 1,
            Self::TimeFrame => 0,
            Self::TimeFrameMenu => 1,
        }
    }
}

impl Ord for UiTarget {
    fn cmp(&self, other: &Self) -> Ordering {
        let ordering = self.zindex().cmp(&other.zindex());
        if ordering == Ordering::Equal {
            (*self as isize).cmp(&(*other as isize))
        } else {
            ordering
        }
    }
}

impl PartialOrd for UiTarget {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
pub struct FrameRateCounter {
    frame_time: AtomicU16,
    frames: AtomicU16,
    last_interval: RwLock<DateTime<Utc>>,
    update_interval: Duration,
}

impl FrameRateCounter {
    pub fn new(update_interval: Duration) -> Self {
        Self {
            frame_time: AtomicU16::new(0),
            frames: AtomicU16::new(0),
            last_interval: RwLock::new(Utc::now()),
            update_interval,
        }
    }

    /// Increments the counter. Returns the frame time if the update interval has elapsed.
    pub fn incr(&self) -> Option<Duration> {
        self.frames.fetch_add(1, atomic::Ordering::Relaxed);

        let now = Utc::now();

        if now >= *self.last_interval.read() + self.update_interval {
            let frames = self.frames.load(atomic::Ordering::Relaxed);
            let frame_time =
                (now - *self.last_interval.read()).num_milliseconds() as f64 / frames as f64;
            let frame_time = round::floor(frame_time, 0) as u16;
            self.frame_time.store(frame_time, atomic::Ordering::Relaxed);

            self.frames.store(0, atomic::Ordering::Relaxed);

            let mut last_interval = self.last_interval.write();
            *last_interval = now;

            return Some(Duration::milliseconds(frame_time as i64));
        }

        None
    }

    pub fn frame_time(&self) -> Option<Duration> {
        match self.frame_time.load(atomic::Ordering::Relaxed) {
            0 => None,
            frame_time => Some(Duration::milliseconds(frame_time as i64)),
        }
    }
}

#[derive(Clone, Copy, Debug, EnumIter, Eq, PartialEq)]
pub enum TimeFrame {
    FiveDays,
    OneMonth,
    ThreeMonths,
    SixMonths,
    YearToDate,
    OneYear,
    TwoYears,
    FiveYears,
    TenYears,
    Max,
}

impl TimeFrame {
    pub fn duration(self) -> Option<Duration> {
        match self {
            Self::FiveDays => Some(Duration::days(5)),
            Self::OneMonth => Some(Duration::days(30)),
            Self::ThreeMonths => Some(Duration::days(30 * 3)),
            Self::SixMonths => Some(Duration::days(30 * 6)),
            Self::OneYear => Some(Duration::days(30 * 12)),
            Self::TwoYears => Some(Duration::days(30 * 12 * 2)),
            Self::FiveYears => Some(Duration::days(30 * 12 * 5)),
            Self::TenYears => Some(Duration::days(30 * 12 * 10)),
            _ => None,
        }
    }

    pub fn interval(self) -> Interval {
        match self {
            Self::FiveDays => Interval::_5d,
            Self::OneMonth => Interval::_1mo,
            Self::ThreeMonths => Interval::_3mo,
            Self::SixMonths => Interval::_6mo,
            Self::YearToDate => Interval::_ytd,
            Self::OneYear => Interval::_1y,
            Self::TwoYears => Interval::_2y,
            Self::FiveYears => Interval::_5y,
            Self::TenYears => Interval::_10y,
            Self::Max => Interval::_max,
        }
    }
}

impl FromStr for TimeFrame {
    type Err = ParseTimeFrameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "5D" | "5d" => Ok(Self::FiveDays),
            "1M" | "1mo" => Ok(Self::OneMonth),
            "3M" | "3mo" => Ok(Self::ThreeMonths),
            "6M" | "6mo" => Ok(Self::SixMonths),
            "YTD" | "ytd" => Ok(Self::YearToDate),
            "1Y" | "1y" => Ok(Self::OneYear),
            "2Y" | "2y" => Ok(Self::TwoYears),
            "5Y" | "5y" => Ok(Self::FiveYears),
            "10Y" | "10y" => Ok(Self::TenYears),
            "MAX" | "max" => Ok(Self::Max),
            "" => Err(ParseTimeFrameError::Empty),
            _ => Err(ParseTimeFrameError::Invalid),
        }
    }
}

#[derive(Debug, Error)]
pub enum ParseTimeFrameError {
    #[error("cannot parse time frame from empty string")]
    Empty,
    #[error("invalid time frame literal")]
    Invalid,
}

impl fmt::Display for TimeFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FiveDays => write!(f, "5D"),
            Self::OneMonth => write!(f, "1M"),
            Self::ThreeMonths => write!(f, "3M"),
            Self::SixMonths => write!(f, "6M"),
            Self::YearToDate => write!(f, "YTD"),
            Self::OneYear => write!(f, "1Y"),
            Self::TwoYears => write!(f, "2Y"),
            Self::FiveYears => write!(f, "5Y"),
            Self::TenYears => write!(f, "10Y"),
            Self::Max => write!(f, "MAX"),
        }
    }
}
