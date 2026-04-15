use crate::{ItemHandle, Pane};
use gpui::{
    AnyView, App, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window,
};
use std::any::TypeId;
use ui::{h_flex, prelude::*};
use util::ResultExt;

pub trait StatusItemView: Render {
    /// Event callback that is triggered when the active pane item changes.
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn crate::ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    );
}

trait StatusItemViewHandle: Send {
    fn to_any(&self) -> AnyView;
    fn set_active_pane_item(
        &self,
        active_pane_item: Option<&dyn ItemHandle>,
        window: &mut Window,
        cx: &mut App,
    );
    fn item_type(&self) -> TypeId;
}

struct StatusItemStrip {
    left_items: Vec<Box<dyn StatusItemViewHandle>>,
    right_items: Vec<Box<dyn StatusItemViewHandle>>,
    active_pane: Entity<Pane>,
}

impl StatusItemStrip {
    fn new(active_pane: &Entity<Pane>) -> Self {
        Self {
            left_items: Default::default(),
            right_items: Default::default(),
            active_pane: active_pane.clone(),
        }
    }

    fn has_items(&self) -> bool {
        !self.left_items.is_empty() || !self.right_items.is_empty()
    }

    fn render_left_tools(&self) -> impl IntoElement {
        h_flex()
            .gap_1()
            .overflow_x_hidden()
            .children(self.left_items.iter().map(|item| item.to_any()))
    }

    fn render_right_tools(&self) -> impl IntoElement {
        h_flex()
            .gap_1()
            .overflow_x_hidden()
            .children(self.right_items.iter().rev().map(|item| item.to_any()))
    }

    fn add_left_item<T>(&mut self, item: Entity<T>, window: &mut Window, cx: &mut App)
    where
        T: 'static + StatusItemView,
    {
        let active_pane_item = self.active_pane.read(cx).active_item();
        item.set_active_pane_item(active_pane_item.as_deref(), window, cx);
        self.left_items.push(Box::new(item));
    }

    fn add_right_item<T>(&mut self, item: Entity<T>, window: &mut Window, cx: &mut App)
    where
        T: 'static + StatusItemView,
    {
        let active_pane_item = self.active_pane.read(cx).active_item();
        item.set_active_pane_item(active_pane_item.as_deref(), window, cx);
        self.right_items.push(Box::new(item));
    }

    fn item_of_type<T: StatusItemView>(&self) -> Option<Entity<T>> {
        self.left_items
            .iter()
            .chain(self.right_items.iter())
            .find_map(|item| item.to_any().downcast().log_err())
    }

    fn position_of_item<T>(&self) -> Option<usize>
    where
        T: StatusItemView,
    {
        for (index, item) in self.left_items.iter().enumerate() {
            if item.item_type() == TypeId::of::<T>() {
                return Some(index);
            }
        }
        for (index, item) in self.right_items.iter().enumerate() {
            if item.item_type() == TypeId::of::<T>() {
                return Some(index + self.left_items.len());
            }
        }
        None
    }

    fn insert_item_after<T>(
        &mut self,
        position: usize,
        item: Entity<T>,
        window: &mut Window,
        cx: &mut App,
    ) where
        T: 'static + StatusItemView,
    {
        let active_pane_item = self.active_pane.read(cx).active_item();
        item.set_active_pane_item(active_pane_item.as_deref(), window, cx);

        if position < self.left_items.len() {
            self.left_items.insert(position + 1, Box::new(item));
        } else {
            self.right_items
                .insert(position + 1 - self.left_items.len(), Box::new(item));
        }
    }

    fn remove_item_at(&mut self, position: usize) {
        if position < self.left_items.len() {
            self.left_items.remove(position);
        } else {
            self.right_items.remove(position - self.left_items.len());
        }
    }

    fn set_active_pane(&mut self, active_pane: &Entity<Pane>) {
        self.active_pane = active_pane.clone();
    }

    fn update_active_pane_item(&mut self, window: &mut Window, cx: &mut App) {
        let active_pane_item = self.active_pane.read(cx).active_item();
        for item in self.left_items.iter().chain(&self.right_items) {
            item.set_active_pane_item(active_pane_item.as_deref(), window, cx);
        }
    }
}

pub struct StatusBar {
    items: StatusItemStrip,
    _observe_active_pane: Subscription,
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .justify_between()
            .gap(DynamicSpacing::Base08.rems(cx))
            .py(DynamicSpacing::Base04.rems(cx))
            .px(DynamicSpacing::Base06.rems(cx))
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().panel_background)
            .child(self.items.render_left_tools())
            .child(self.items.render_right_tools())
    }
}

impl StatusBar {
    pub fn new(active_pane: &Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            items: StatusItemStrip::new(active_pane),
            _observe_active_pane: cx.observe_in(active_pane, window, |this, _, window, cx| {
                this.items.update_active_pane_item(window, cx)
            }),
        };
        this.items.update_active_pane_item(window, cx);
        this
    }

    pub fn has_items(&self) -> bool {
        self.items.has_items()
    }

    pub fn add_left_item<T>(&mut self, item: Entity<T>, window: &mut Window, cx: &mut Context<Self>)
    where
        T: 'static + StatusItemView,
    {
        self.items.add_left_item(item, window, cx);
        cx.notify();
    }

    pub fn item_of_type<T: StatusItemView>(&self) -> Option<Entity<T>> {
        self.items.item_of_type::<T>()
    }

    pub fn position_of_item<T>(&self) -> Option<usize>
    where
        T: StatusItemView,
    {
        self.items.position_of_item::<T>()
    }

    pub fn insert_item_after<T>(
        &mut self,
        position: usize,
        item: Entity<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        T: 'static + StatusItemView,
    {
        self.items.insert_item_after(position, item, window, cx);
        cx.notify();
    }

    pub fn remove_item_at(&mut self, position: usize, cx: &mut Context<Self>) {
        self.items.remove_item_at(position);
        cx.notify();
    }

    pub fn add_right_item<T>(
        &mut self,
        item: Entity<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        T: 'static + StatusItemView,
    {
        self.items.add_right_item(item, window, cx);
        cx.notify();
    }

    pub fn set_active_pane(
        &mut self,
        active_pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.items.set_active_pane(active_pane);
        self._observe_active_pane = cx.observe_in(active_pane, window, |this, _, window, cx| {
            this.items.update_active_pane_item(window, cx)
        });
        self.items.update_active_pane_item(window, cx);
    }
}

impl<T: StatusItemView> StatusItemViewHandle for Entity<T> {
    fn to_any(&self) -> AnyView {
        self.clone().into()
    }

    fn set_active_pane_item(
        &self,
        active_pane_item: Option<&dyn ItemHandle>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.update(cx, |this, cx| {
            this.set_active_pane_item(active_pane_item, window, cx)
        });
    }

    fn item_type(&self) -> TypeId {
        TypeId::of::<T>()
    }
}

impl From<&dyn StatusItemViewHandle> for AnyView {
    fn from(val: &dyn StatusItemViewHandle) -> Self {
        val.to_any()
    }
}
