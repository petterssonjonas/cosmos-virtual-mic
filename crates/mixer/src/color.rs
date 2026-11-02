// SPDX-License-Identifier: MPL-2.0
use crate::app::Message;
use cosmic::iced::{Color, Point, Rectangle, Renderer, mouse};
use cosmic::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};

pub struct ColorPad {
    pub index: usize,
    pub warmth: f32,
    pub brightness: f32,
}

impl canvas::Program<Message, cosmic::Theme> for ColorPad {
    type State = bool;
    fn update(
        &self,
        dragging: &mut bool,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<cosmic::iced::widget::Action<Message>> {
        use mouse::Event;
        match event {
            canvas::Event::Mouse(Event::ButtonPressed(mouse::Button::Left))
                if cursor.is_over(bounds) =>
            {
                *dragging = true
            }
            canvas::Event::Mouse(Event::ButtonReleased(mouse::Button::Left)) => {
                *dragging = false;
                return None;
            }
            canvas::Event::Mouse(Event::CursorMoved { .. }) if *dragging => {}
            _ => return None,
        }
        let point = cursor.position()?;
        let warmth = ((point.x - bounds.x) / bounds.width * 24.0 - 12.0).clamp(-12.0, 12.0);
        let brightness = (12.0 - (point.y - bounds.y) / bounds.height * 24.0).clamp(-12.0, 12.0);
        Some(
            cosmic::iced::widget::Action::publish(Message::Color(self.index, warmth, brightness))
                .and_capture(),
        )
    }
    fn draw(
        &self,
        _: &bool,
        renderer: &Renderer,
        theme: &cosmic::Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let palette = theme.cosmic();
        let ink: Color = palette.background(false).on.into();
        let grid: Color = palette.background(false).divider.into();
        let accent: Color = palette.accent.base.into();
        frame.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Color::from(palette.primary(false).base),
        );
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            frame.stroke(
                &Path::line(
                    Point::new(bounds.width * fraction, 0.0),
                    Point::new(bounds.width * fraction, bounds.height),
                ),
                Stroke::default().with_color(grid),
            );
            frame.stroke(
                &Path::line(
                    Point::new(0.0, bounds.height * fraction),
                    Point::new(bounds.width, bounds.height * fraction),
                ),
                Stroke::default().with_color(grid),
            );
        }
        frame.fill_text(Text {
            content: "BRIGHT".into(),
            position: Point::new(8.0, 5.0),
            color: ink,
            size: 10.0.into(),
            ..Text::default()
        });
        frame.fill_text(Text {
            content: "WARM →".into(),
            position: Point::new(bounds.width - 62.0, bounds.height - 19.0),
            color: ink,
            size: 10.0.into(),
            ..Text::default()
        });
        let center = Point::new(
            (self.warmth + 12.0) / 24.0 * bounds.width,
            (12.0 - self.brightness) / 24.0 * bounds.height,
        );
        frame.fill(&Path::circle(center, 7.0), accent);
        frame.stroke(
            &Path::circle(center, 9.0),
            Stroke::default().with_color(ink).with_width(1.0),
        );
        vec![frame.into_geometry()]
    }
    fn mouse_interaction(
        &self,
        dragging: &bool,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if *dragging || cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}
