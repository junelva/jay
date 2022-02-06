use crate::backend::{KeyState, OutputId, ScrollAxis, SeatEvent, SeatId};
use crate::client::ClientId;
use crate::fixed::Fixed;
use crate::ifs::wl_data_device::WlDataDevice;
use crate::ifs::wl_seat::wl_keyboard::WlKeyboard;
use crate::ifs::wl_seat::wl_pointer::{WlPointer, POINTER_FRAME_SINCE_VERSION};
use crate::ifs::wl_seat::{
    wl_keyboard, wl_pointer, PointerGrab, PointerGrabber, WlSeat, WlSeatGlobal,
};
use crate::ifs::wl_surface::xdg_surface::xdg_popup::XdgPopup;
use crate::ifs::wl_surface::xdg_surface::xdg_toplevel::XdgToplevel;
use crate::ifs::wl_surface::xdg_surface::XdgSurface;
use crate::ifs::wl_surface::WlSurface;
use crate::ifs::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1;
use crate::tree::{FloatNode, FoundNode, Node};
use crate::utils::smallmap::SmallMap;
use crate::wire::{WlDataOfferId, ZwpPrimarySelectionOfferV1Id};
use crate::xkbcommon::{ModifierState, XKB_KEY_DOWN, XKB_KEY_UP};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

#[derive(Default)]
pub struct NodeSeatState {
    pointer_foci: SmallMap<SeatId, Rc<WlSeatGlobal>, 1>,
    kb_foci: SmallMap<SeatId, Rc<WlSeatGlobal>, 1>,
    grabs: SmallMap<SeatId, PointerGrab, 1>,
}

impl NodeSeatState {
    fn enter(&self, seat: &Rc<WlSeatGlobal>) {
        self.pointer_foci.insert(seat.seat.id(), seat.clone());
    }

    fn leave(&self, seat: &WlSeatGlobal) {
        self.pointer_foci.remove(&seat.seat.id());
    }

    fn focus(&self, seat: &Rc<WlSeatGlobal>) -> bool {
        self.kb_foci.insert(seat.seat.id(), seat.clone());
        self.kb_foci.len() == 1
    }

    fn unfocus(&self, seat: &WlSeatGlobal) -> bool {
        self.kb_foci.remove(&seat.seat.id());
        self.kb_foci.len() == 0
    }

    fn add_pointer_grab(&self, seat: &Rc<WlSeatGlobal>) {
        self.grabs
            .insert(seat.id(), PointerGrab { seat: seat.clone() });
    }

    fn remove_pointer_grab(&self, seat: &WlSeatGlobal) {
        self.grabs.remove(&seat.id());
    }

    // pub fn remove_pointer_grabs(&self) {
    //     self.grabs.clear();
    // }

    pub fn is_active(&self) -> bool {
        self.kb_foci.len() > 0
    }

    pub fn destroy_node(&self, node: &dyn Node) {
        self.grabs.clear();
        let node_id = node.id();
        while let Some((_, seat)) = self.pointer_foci.pop() {
            let mut ps = seat.pointer_stack.borrow_mut();
            while let Some(last) = ps.pop() {
                if last.id() == node_id {
                    break;
                }
            }
            seat.state.tree_changed();
        }
        while let Some((_, seat)) = self.kb_foci.pop() {
            seat.keyboard_node.set(seat.state.root.clone());
            if let Some(tl) = seat.toplevel_focus_history.last() {
                seat.focus_xdg_surface(&tl.xdg);
            }
        }
    }
}

impl WlSeatGlobal {
    pub fn event(self: &Rc<Self>, event: SeatEvent) {
        match event {
            SeatEvent::OutputPosition(o, x, y) => self.output_position_event(o, x, y),
            SeatEvent::Motion(dx, dy) => self.motion_event(dx, dy),
            SeatEvent::Button(b, s) => self.button_event(b, s),
            SeatEvent::Scroll(d, a) => self.scroll_event(d, a),
            SeatEvent::Key(k, s) => self.key_event(k, s),
        }
    }

    fn output_position_event(self: &Rc<Self>, output: OutputId, mut x: Fixed, mut y: Fixed) {
        let output = match self.state.outputs.get(&output) {
            Some(o) => o,
            _ => return,
        };
        x += Fixed::from_int(output.x.get());
        y += Fixed::from_int(output.y.get());
        self.set_new_position(x, y);
    }

    fn motion_event(self: &Rc<Self>, dx: Fixed, dy: Fixed) {
        let (x, y) = self.pos.get();
        self.set_new_position(x + dx, y + dy);
    }

    fn button_event(self: &Rc<Self>, button: u32, state: KeyState) {
        let mut release_grab = false;
        let mut grabber = self.grabber.borrow_mut();
        let node = if let Some(pg) = grabber.deref_mut() {
            if state == KeyState::Released {
                pg.buttons.remove(&button);
                if pg.buttons.is_empty() {
                    release_grab = true;
                }
            } else {
                pg.buttons.insert(button, ());
            }
            pg.node.clone()
        } else if state == KeyState::Pressed {
            match self.pointer_node() {
                Some(n) => {
                    *grabber = Some(PointerGrabber {
                        node: n.clone(),
                        buttons: SmallMap::new_with(button, ()),
                    });
                    n.seat_state().add_pointer_grab(self);
                    n
                }
                _ => return,
            }
        } else {
            return;
        };
        drop(grabber);
        if release_grab {
            node.seat_state().remove_pointer_grab(self);
        }
        node.button(self, button, state);
    }

    fn scroll_event(&self, delta: i32, axis: ScrollAxis) {
        let node = match self.grabber.borrow_mut().as_ref().map(|g| g.node.clone()) {
            Some(n) => n,
            _ => match self.pointer_node() {
                Some(n) => n,
                _ => return,
            },
        };
        node.scroll(self, delta, axis);
    }

    fn key_event(&self, key: u32, state: KeyState) {
        let (state, xkb_dir) = {
            let mut pk = self.pressed_keys.borrow_mut();
            match state {
                KeyState::Released => {
                    if !pk.remove(&key) {
                        return;
                    }
                    (wl_keyboard::RELEASED, XKB_KEY_UP)
                }
                KeyState::Pressed => {
                    if !pk.insert(key) {
                        return;
                    }
                    (wl_keyboard::PRESSED, XKB_KEY_DOWN)
                }
            }
        };
        let mods = self.kb_state.borrow_mut().update(key, xkb_dir);
        let node = self.keyboard_node.get();
        node.key(self, key, state, mods);
    }
}

impl WlSeatGlobal {
    fn pointer_node(&self) -> Option<Rc<dyn Node>> {
        self.pointer_stack.borrow().last().cloned()
    }

    pub fn last_tiled_keyboard_toplevel(&self) -> Option<Rc<XdgToplevel>> {
        for tl in self.toplevel_focus_history.rev_iter() {
            if !tl.parent_is_float() {
                return Some(tl.deref().clone());
            }
        }
        None
    }

    pub fn move_(&self, node: &Rc<FloatNode>) {
        self.move_.set(true);
        self.move_start_pos.set(self.pos.get());
        let ex = node.position.get();
        self.extents_start_pos.set((ex.x1(), ex.y1()));
    }

    pub fn focus_toplevel(self: &Rc<Self>, n: &Rc<XdgToplevel>) {
        let node = self.toplevel_focus_history.add_last(n.clone());
        n.toplevel_history.insert(self.id(), node);
        self.focus_xdg_surface(&n.xdg);
    }

    fn focus_xdg_surface(self: &Rc<Self>, xdg: &Rc<XdgSurface>) {
        self.focus_surface(&xdg.focus_surface(self));
    }

    fn focus_surface(self: &Rc<Self>, surface: &Rc<WlSurface>) {
        let old = self.keyboard_node.get();
        if old.id() == surface.node_id {
            return;
        }
        old.unfocus(self);
        if old.seat_state().unfocus(self) {
            old.active_changed(false);
        }

        if surface.seat_state().focus(self) {
            surface.active_changed(true);
        }
        surface.clone().focus(self);
        self.keyboard_node.set(surface.clone());

        let pressed_keys: Vec<_> = self.pressed_keys.borrow().iter().copied().collect();
        let serial = self.serial.fetch_add(1);
        self.surface_kb_event(0, &surface, |k| {
            k.send_enter(serial, surface.id, &pressed_keys)
        });
        let ModifierState {
            mods_depressed,
            mods_latched,
            mods_locked,
            group,
        } = self.kb_state.borrow().mods();
        let serial = self.serial.fetch_add(1);
        self.surface_kb_event(0, &surface, |k| {
            k.send_modifiers(serial, mods_depressed, mods_latched, mods_locked, group)
        });

        if old.client_id() != Some(surface.client.id) {
            match self.selection.get() {
                None => {
                    self.surface_data_device_event(0, &surface, |dd| {
                        dd.send_selection(WlDataOfferId::NONE)
                    });
                }
                Some(sel) => {
                    sel.create_offer(&surface.client);
                }
            }
            match self.primary_selection.get() {
                None => {
                    self.surface_primary_selection_device_event(0, &surface, |dd| {
                        dd.send_selection(ZwpPrimarySelectionOfferV1Id::NONE)
                    });
                }
                Some(sel) => {
                    sel.create_offer(&surface.client);
                }
            }
        }
    }

    fn for_each_seat<C>(&self, ver: u32, client: ClientId, mut f: C)
    where
        C: FnMut(&Rc<WlSeat>),
    {
        let bindings = self.bindings.borrow();
        if let Some(hm) = bindings.get(&client) {
            for seat in hm.values() {
                if seat.version >= ver {
                    f(seat);
                }
            }
        }
    }

    fn for_each_pointer<C>(&self, ver: u32, client: ClientId, mut f: C)
    where
        C: FnMut(&Rc<WlPointer>),
    {
        self.for_each_seat(ver, client, |seat| {
            let pointers = seat.pointers.lock();
            for pointer in pointers.values() {
                f(pointer);
            }
        })
    }

    fn for_each_kb<C>(&self, ver: u32, client: ClientId, mut f: C)
    where
        C: FnMut(&Rc<WlKeyboard>),
    {
        self.for_each_seat(ver, client, |seat| {
            let keyboards = seat.keyboards.lock();
            for keyboard in keyboards.values() {
                f(keyboard);
            }
        })
    }

    pub fn for_each_data_device<C>(&self, ver: u32, client: ClientId, mut f: C)
    where
        C: FnMut(&Rc<WlDataDevice>),
    {
        let dd = self.data_devices.borrow_mut();
        if let Some(dd) = dd.get(&client) {
            for dd in dd.values() {
                if dd.manager.version >= ver {
                    f(dd);
                }
            }
        }
    }

    pub fn for_each_primary_selection_device<C>(&self, ver: u32, client: ClientId, mut f: C)
    where
        C: FnMut(&Rc<ZwpPrimarySelectionDeviceV1>),
    {
        let dd = self.primary_selection_devices.borrow_mut();
        if let Some(dd) = dd.get(&client) {
            for dd in dd.values() {
                if dd.manager.version >= ver {
                    f(dd);
                }
            }
        }
    }

    fn surface_pointer_frame(&self, surface: &WlSurface) {
        self.surface_pointer_event(POINTER_FRAME_SINCE_VERSION, surface, |p| p.send_frame());
    }

    fn surface_pointer_event<F>(&self, ver: u32, surface: &WlSurface, mut f: F)
    where
        F: FnMut(&Rc<WlPointer>),
    {
        let client = &surface.client;
        self.for_each_pointer(ver, client.id, |p| {
            f(p);
        });
        client.flush();
    }

    fn surface_kb_event<F>(&self, ver: u32, surface: &WlSurface, mut f: F)
    where
        F: FnMut(&Rc<WlKeyboard>),
    {
        let client = &surface.client;
        self.for_each_kb(ver, client.id, |p| {
            f(p);
        });
        client.flush();
    }

    fn surface_data_device_event<F>(&self, ver: u32, surface: &WlSurface, mut f: F)
    where
        F: FnMut(&Rc<WlDataDevice>),
    {
        let client = &surface.client;
        self.for_each_data_device(ver, client.id, |p| {
            f(p);
        });
        client.flush();
    }

    fn surface_primary_selection_device_event<F>(&self, ver: u32, surface: &WlSurface, mut f: F)
    where
        F: FnMut(&Rc<ZwpPrimarySelectionDeviceV1>),
    {
        let client = &surface.client;
        self.for_each_primary_selection_device(ver, client.id, |p| {
            f(p);
        });
        client.flush();
    }

    fn set_new_position(self: &Rc<Self>, x: Fixed, y: Fixed) {
        self.pos.set((x, y));
        self.handle_new_position(true);
    }

    pub fn tree_changed(self: &Rc<Self>) {
        self.handle_new_position(false);
    }

    fn handle_new_position(self: &Rc<Self>, pos_changed: bool) {
        let (x, y) = self.pos.get();
        if pos_changed {
            if let Some(cursor) = self.cursor.get() {
                cursor.set_position(x.round_down(), y.round_down());
            }
        }
        'handle_grab: {
            let grab_node = {
                let grabber = self.grabber.borrow_mut();
                match grabber.as_ref() {
                    Some(n) => n.node.clone(),
                    None => break 'handle_grab,
                }
            };
            if pos_changed {
                let pos = grab_node.absolute_position();
                let (x_int, y_int) = pos.translate(x.round_down(), y.round_down());
                grab_node.motion(self, x.apply_fract(x_int), y.apply_fract(y_int));
            }
            return;
        }
        let mut found_tree = self.found_tree.borrow_mut();
        let mut stack = self.pointer_stack.borrow_mut();
        // if self.move_.get() {
        //     for node in stack.iter().rev() {
        //         if let NodeKind::Toplevel(tn) = node.clone().into_kind() {
        //             let (move_start_x, move_start_y) = self.move_start_pos.get();
        //             let (move_start_ex, move_start_ey) = self.extents_start_pos.get();
        //             let mut ex = tn.common.extents.get();
        //             ex.x = (x - move_start_x).round_down() + move_start_ex;
        //             ex.y = (y - move_start_y).round_down() + move_start_ey;
        //             tn.common.extents.set(ex);
        //         }
        //     }
        //     return;
        // }
        let x_int = x.round_down();
        let y_int = y.round_down();
        found_tree.push(FoundNode {
            node: self.state.root.clone(),
            x: x_int,
            y: y_int,
        });
        self.state.root.find_tree_at(x_int, y_int, &mut found_tree);
        let mut divergence = found_tree.len().min(stack.len());
        for (i, (found, stack)) in found_tree.iter().zip(stack.iter()).enumerate() {
            if found.node.id() != stack.id() {
                divergence = i;
                break;
            }
        }
        if (stack.len(), found_tree.len()) == (divergence, divergence) {
            if pos_changed {
                if let Some(node) = found_tree.last() {
                    node.node
                        .motion(self, x.apply_fract(node.x), y.apply_fract(node.y));
                }
            }
        } else {
            if let Some(last) = stack.last() {
                last.pointer_untarget(self);
            }
            for old in stack.drain(divergence..).rev() {
                old.leave(self);
                old.seat_state().leave(self);
            }
            if found_tree.len() == divergence {
                if let Some(node) = found_tree.last() {
                    node.node
                        .clone()
                        .motion(self, x.apply_fract(node.x), y.apply_fract(node.y));
                }
            } else {
                for new in found_tree.drain(divergence..) {
                    new.node.seat_state().enter(self);
                    new.node
                        .clone()
                        .enter(self, x.apply_fract(new.x), y.apply_fract(new.y));
                    stack.push(new.node);
                }
            }
            if let Some(node) = stack.last() {
                node.pointer_target(self);
            }
        }
        found_tree.clear();
    }
}

// Button callbacks
impl WlSeatGlobal {
    pub fn button_surface(self: &Rc<Self>, surface: &Rc<WlSurface>, button: u32, state: KeyState) {
        let (state, pressed) = match state {
            KeyState::Released => (wl_pointer::RELEASED, false),
            KeyState::Pressed => (wl_pointer::PRESSED, true),
        };
        let serial = self.serial.fetch_add(1);
        self.surface_pointer_event(0, surface, |p| p.send_button(serial, 0, button, state));
        self.surface_pointer_frame(surface);
        if pressed && surface.belongs_to_toplevel() {
            self.focus_surface(surface);
        }
    }
}

// Scroll callbacks
impl WlSeatGlobal {
    pub fn scroll_surface(&self, surface: &WlSurface, delta: i32, axis: ScrollAxis) {
        let axis = match axis {
            ScrollAxis::Horizontal => wl_pointer::HORIZONTAL_SCROLL,
            ScrollAxis::Vertical => wl_pointer::VERTICAL_SCROLL,
        };
        self.surface_pointer_event(0, surface, |p| p.send_axis(0, axis, Fixed::from_int(delta)));
        self.surface_pointer_frame(surface);
    }
}

// Motion callbacks
impl WlSeatGlobal {
    pub fn motion_surface(&self, n: &WlSurface, x: Fixed, y: Fixed) {
        self.surface_pointer_event(0, n, |p| p.send_motion(0, x, y));
        self.surface_pointer_frame(n);
    }
}

// Enter callbacks
impl WlSeatGlobal {
    pub fn enter_toplevel(self: &Rc<Self>, n: &Rc<XdgToplevel>) {
        self.focus_toplevel(n);
    }

    pub fn enter_popup(self: &Rc<Self>, _n: &Rc<XdgPopup>) {
        // self.focus_xdg_surface(&n.xdg);
    }

    pub fn enter_surface(&self, n: &WlSurface, x: Fixed, y: Fixed) {
        let serial = self.serial.fetch_add(1);
        self.surface_pointer_event(0, n, |p| p.send_enter(serial, n.id, x, y));
        self.surface_pointer_frame(n);
    }
}

// Leave callbacks
impl WlSeatGlobal {
    pub fn leave_surface(&self, n: &WlSurface) {
        let serial = self.serial.fetch_add(1);
        self.surface_pointer_event(0, n, |p| p.send_leave(serial, n.id));
        self.surface_pointer_frame(n);
    }
}

// Unfocus callbacks
impl WlSeatGlobal {
    pub fn unfocus_surface(&self, surface: &WlSurface) {
        self.surface_kb_event(0, surface, |k| k.send_leave(0, surface.id))
    }
}

// Key callbacks
impl WlSeatGlobal {
    pub fn key_surface(
        &self,
        surface: &WlSurface,
        key: u32,
        state: u32,
        mods: Option<ModifierState>,
    ) {
        let serial = self.serial.fetch_add(1);
        self.surface_kb_event(0, surface, |k| k.send_key(serial, 0, key, state));
        let serial = self.serial.fetch_add(1);
        if let Some(mods) = mods {
            self.surface_kb_event(0, surface, |k| {
                k.send_modifiers(
                    serial,
                    mods.mods_depressed,
                    mods.mods_latched,
                    mods.mods_locked,
                    mods.group,
                )
            });
        }
    }
}