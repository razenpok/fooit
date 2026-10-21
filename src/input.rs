//! Single line text input, ported from dooit's `Input` widget

pub const CURSOR: char = '|';

#[derive(Clone, Debug)]
pub struct Input {
    value: Vec<char>,
    cursor: usize,
    pub editing: bool,
}

impl Input {
    pub fn new(value: &str) -> Input {
        let value: Vec<char> = value.chars().collect();
        Input {
            cursor: value.len(),
            value,
            editing: true,
        }
    }

    pub fn value(&self) -> String {
        self.value.iter().collect()
    }

    /// Text with the cursor drawn into it while editing
    pub fn draw(&self) -> String {
        if !self.editing {
            return self.value();
        }
        let mut text: String = self.value[..self.cursor].iter().collect();
        text.push(CURSOR);
        text.extend(&self.value[self.cursor..]);
        text
    }

    pub fn render(&self) -> String {
        self.draw().trim().to_string()
    }

    /// Put the cursor before the character at an index, or at the end
    pub fn set_cursor(&mut self, index: usize) {
        self.cursor = index.min(self.value.len());
    }

    pub fn insert_text(&mut self, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        let count = chars.len();
        self.value.splice(self.cursor..self.cursor, chars);
        self.cursor += count;
    }

    fn move_backward(&mut self, word: bool, delete: bool) {
        let prev = self.cursor;

        if !word {
            self.cursor = self.cursor.saturating_sub(1);
        } else {
            while self.cursor > 0 {
                if self.value[self.cursor - 1] != ' '
                    && (self.cursor == 1 || self.value[self.cursor - 2] == ' ')
                {
                    self.cursor -= 1;
                    break;
                }
                self.cursor -= 1;
            }
        }

        if delete {
            self.value.drain(self.cursor..prev);
        }
    }

    fn move_forward(&mut self, word: bool, delete: bool) {
        let prev = self.cursor;
        let len = self.value.len();

        if !word {
            self.cursor = (self.cursor + 1).min(len);
        } else {
            while self.cursor < len {
                if self.cursor != prev
                    && self.value[self.cursor - 1] == ' '
                    && (self.cursor == len - 1 || self.value[self.cursor] != ' ')
                {
                    break;
                }
                self.cursor += 1;
            }
        }

        if delete {
            self.value.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    pub fn keypress(&mut self, key: &str) {
        match key {
            "left" => self.move_backward(false, false),
            "ctrl+left" => self.move_backward(true, false),
            "backspace" => self.move_backward(false, true),
            "ctrl+w" => self.move_backward(true, true),
            "right" => self.move_forward(false, false),
            "ctrl+right" => self.move_forward(true, false),
            "delete" => self.move_forward(false, true),
            "ctrl+delete" => self.move_forward(true, true),
            "ctrl+l" => self.clear(),
            "home" | "ctrl+a" => self.cursor = 0,
            "end" | "ctrl+e" => self.cursor = self.value.len(),
            "tab" => self.insert_text("\t"),
            "space" => self.insert_text(" "),
            _ if key.chars().count() == 1 => self.insert_text(key),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing() {
        let mut i = Input::new("hello world");
        assert_eq!(i.draw(), "hello world|");
        i.keypress("ctrl+w");
        assert_eq!(i.value(), "hello ");
        i.keypress("ctrl+left");
        assert_eq!(i.draw(), "|hello ");
        i.keypress("x");
        assert_eq!(i.draw(), "x|hello ");
        i.keypress("delete");
        assert_eq!(i.value(), "xello ");
        i.keypress("ctrl+delete");
        assert_eq!(i.value(), "x");
        i.keypress("backspace");
        i.keypress("backspace");
        assert_eq!(i.value(), "");
        i.keypress("é");
        assert_eq!(i.render(), "é|");
    }
}
