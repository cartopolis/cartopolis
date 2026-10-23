//! The agent's speech on its way into the call, and what was actually heard.
//!
//! The model streams a reply faster than real time, so its audio waits here
//! and leaves at 50 frames a second. When somebody talks over it the rest is
//! thrown away, and the model has to be told how much of the reply was really
//! played (`conversation.item.truncate`), or it carries on believing it said
//! the whole thing.

use std::collections::VecDeque;

use crate::FRAME;

#[derive(Default)]
pub struct Playout {
    /// Reply audio at the call's rate, each run tagged with its item.
    queue: VecDeque<(String, Vec<f32>)>,
    /// Samples taken from the front run so far.
    front_taken: usize,
    /// The item playing now and how many of its samples have been played.
    playing: Option<(String, usize)>,
}

impl Playout {
    /// More of `item`'s audio, at the call's rate.
    pub fn push(&mut self, item: &str, samples: Vec<f32>) {
        if samples.is_empty() {
            return;
        }
        match self.queue.back_mut() {
            Some((id, run)) if id == item => run.extend(samples),
            _ => self.queue.push_back((item.to_string(), samples)),
        }
    }

    /// The next 20 ms to send, or `None` while there is nothing to say. A
    /// reply that ends mid-frame is padded with silence.
    pub fn next_frame(&mut self) -> Option<Vec<f32>> {
        if self.queue.is_empty() {
            return None;
        }
        let mut frame = Vec::with_capacity(FRAME);
        while frame.len() < FRAME {
            let Some((id, run)) = self.queue.front() else {
                break;
            };
            let take = (FRAME - frame.len()).min(run.len() - self.front_taken);
            frame.extend_from_slice(&run[self.front_taken..self.front_taken + take]);
            self.front_taken += take;
            match &mut self.playing {
                Some((p, n)) if p == id => *n += take,
                _ => self.playing = Some((id.clone(), take)),
            }
            if self.front_taken == run.len() {
                self.queue.pop_front();
                self.front_taken = 0;
            }
        }
        frame.resize(FRAME, 0.0);
        Some(frame)
    }

    /// Whether anything is still waiting to be said.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Somebody talked over the agent: drop what is waiting, and say which
    /// item was cut and after how many milliseconds of it, when one was. A
    /// reply that had already been played to its end was not interrupted.
    pub fn interrupt(&mut self, rate: u32) -> Option<(String, u32)> {
        let cut = self.queue.front().map(|(id, _)| id.clone())?;
        self.queue.clear();
        self.front_taken = 0;
        let played = match &self.playing {
            Some((id, n)) if *id == cut => *n,
            _ => 0,
        };
        self.playing = None;
        Some((cut, (played as u64 * 1000 / u64::from(rate)) as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_leaves_in_whole_frames_and_says_what_was_played() {
        let mut p = Playout::default();
        assert!(p.next_frame().is_none());
        p.push("a", vec![0.1; 500]);
        p.push("a", vec![0.1; 300]);
        assert_eq!(p.next_frame().map(|f| f.len()), Some(FRAME));
        // 800 samples: two whole frames and a padded third.
        let second = p.next_frame().expect("a second frame");
        assert!(second.iter().all(|s| *s == 0.1));
        let third = p.next_frame().expect("a padded third");
        assert_eq!(third[159], 0.1);
        assert_eq!(third[160], 0.0);
        assert!(p.next_frame().is_none());
        // Played to its end: nothing was cut.
        assert_eq!(p.interrupt(16_000), None);
    }

    #[test]
    fn an_interruption_reports_the_cut_item_and_how_much_was_heard() {
        let mut p = Playout::default();
        p.push("a", vec![0.1; 320]);
        p.push("b", vec![0.2; 16_000]);
        p.next_frame(); // all of a
        p.next_frame(); // 20 ms of b
        p.next_frame(); // 40 ms of b
        assert_eq!(p.interrupt(16_000), Some(("b".into(), 40)));
        assert!(p.is_empty());
        assert!(p.next_frame().is_none());
    }

    #[test]
    fn an_item_cut_before_it_started_was_heard_for_no_time() {
        let mut p = Playout::default();
        p.push("a", vec![0.1; 320]);
        p.next_frame();
        p.push("b", vec![0.2; 320]);
        assert_eq!(p.interrupt(16_000), Some(("b".into(), 0)));
    }
}
