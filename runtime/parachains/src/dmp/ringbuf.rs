// Copyright 2020 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.

//! Implements API for managing a ring buffer and an associated message window.

use frame_support::pallet_prelude::*;
use polkadot_parachain::primitives::{MessageIndex, PageIndex, WrappingIndex};
use primitives::v2::Id as ParaId;
use sp_std::prelude::*;

/// Unique identifier of an inbound downward message.
#[derive(Encode, Decode, Clone, Default, Copy, sp_runtime::RuntimeDebug, PartialEq, TypeInfo)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct ParaMessageIndex {
	/// The recipient parachain.
	pub para_id: ParaId,
	/// A message index in the recipient parachain queue.
	pub message_idx: WrappingIndex<MessageIndex>,
}

/// The key for a queue page of a parachain.
#[derive(Encode, Decode, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo)]
pub struct QueuePageIndex {
	/// The recipient parachain.
	pub para_id: ParaId,
	/// The page index.
	pub page_idx: WrappingIndex<PageIndex>,
}

/// The state of the message window. The message window is used to provide a 1:1 mapping to the
/// messages stored in the ring buffer.
///
/// Invariants:
/// - the window size is always equal to the amount of messages stored in the ring buffer.
#[derive(Encode, Decode, Default, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo)]
pub struct MessageWindowState {
	/// The first used index corresponding to first message in the queue.
	first_message_idx: WrappingIndex<MessageIndex>,
	/// The first free index.
	free_message_idx: WrappingIndex<MessageIndex>,
}

/// The state of the ring buffer that represents the message queue. We only need to keep track
/// of the first used(head) and unused(tail) pages.
/// Invariants:
///  - the window size is always equal to the queue size.
#[derive(Encode, Decode, Default, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo)]
pub struct RingBufferState {
	/// The index of the first used page.
	head_page_idx: WrappingIndex<PageIndex>,
	/// The index of the first unused page. `tail_page_idx - 1` is the last used page.
	tail_page_idx: WrappingIndex<PageIndex>,
}

#[cfg(test)]
impl RingBufferState {
	pub fn new(
		head_page_idx: WrappingIndex<PageIndex>,
		tail_page_idx: WrappingIndex<PageIndex>,
	) -> RingBufferState {
		RingBufferState { tail_page_idx, head_page_idx }
	}
}

/// Manages the downward message indexing window. All downward messages are assigned
/// an index when they are queued.
pub struct MessageWindow {
	para_id: ParaId,
	state: MessageWindowState,
}

#[derive(Clone, Copy)]
/// Provides basic methods to interact with the ring buffer.
pub struct RingBuffer {
	para_id: ParaId,
	state: RingBufferState,
}

/// An iterator over the collection of pages in the ring buffer.
pub struct RingBufferIterator(RingBuffer);

impl IntoIterator for RingBuffer {
	type Item = QueuePageIndex;
	type IntoIter = RingBufferIterator;

	fn into_iter(self) -> Self::IntoIter {
		RingBufferIterator(self)
	}
}

impl Iterator for RingBufferIterator {
	type Item = QueuePageIndex;

	fn next(&mut self) -> Option<Self::Item> {
		self.0.pop_front()
	}
}

impl RingBuffer {
	pub fn with_state(state: RingBufferState, para_id: ParaId) -> RingBuffer {
		RingBuffer { state, para_id }
	}

	/// Allocates a new page and returns the page index.
	/// Panics if there are no free pages.
	pub fn extend(&mut self) -> QueuePageIndex {
		// In practice this is always bounded economically - sending a message requires paying fee/deposit.
		if self.state.tail_page_idx.wrapping_inc() == self.state.head_page_idx {
			panic!("The end of the world is upon us");
		}

		// Advance tail to the next unused page.
		self.state.tail_page_idx = self.state.tail_page_idx.wrapping_inc();
		// Return last used page.
		QueuePageIndex { para_id: self.para_id, page_idx: self.state.tail_page_idx.wrapping_dec() }
	}

	/// Frees up to count `pages` by advacing the head page index. If count is larger than
	/// the size of the ring buffer, the head page index will be equal to first free page index
	/// meaning the buffer is empty.
	pub fn prune(&mut self, count: u32) {
		// Ensure we don't overflow and the head overtakes the tail.
		let to_prune = sp_std::cmp::min(self.size(), count as u64);

		// Advance head by `count` pages.
		self.state.head_page_idx = self.state.head_page_idx.wrapping_add(to_prune.into());
	}

	/// Frees the first used page and returns it's index while advacing the head of the ring buffer.
	/// If the queue is empty it does nothing and returns `None`.
	pub fn pop_front(&mut self) -> Option<QueuePageIndex> {
		let page = self.front();

		if page.is_some() {
			self.state.head_page_idx = self.state.head_page_idx.wrapping_inc();
		}

		page
	}

	/// Returns the first page or `None` if ring buffer empty.
	pub fn front(&self) -> Option<QueuePageIndex> {
		if self.state.tail_page_idx == self.state.head_page_idx {
			None
		} else {
			Some(QueuePageIndex { para_id: self.para_id, page_idx: self.state.head_page_idx })
		}
	}

	/// Returns the last used page or `None` if ring buffer empty.
	pub fn last_used(&self) -> Option<QueuePageIndex> {
		if self.state.tail_page_idx == self.state.head_page_idx {
			None
		} else {
			Some(QueuePageIndex {
				para_id: self.para_id,
				page_idx: self.state.tail_page_idx.wrapping_dec(),
			})
		}
	}

	#[cfg(test)]
	pub fn first_unused(&self) -> QueuePageIndex {
		QueuePageIndex { para_id: self.para_id, page_idx: self.state.tail_page_idx }
	}

	/// Returns the size in pages.
	pub fn size(&self) -> u64 {
		self.state.tail_page_idx.wrapping_sub(self.state.head_page_idx).into()
	}

	/// Returns the wrapped state.
	pub fn into_inner(self) -> RingBufferState {
		self.state
	}
}

impl MessageWindow {
	/// Construct from state of a given para.
	pub fn with_state(state: MessageWindowState, para_id: ParaId) -> MessageWindow {
		MessageWindow { para_id, state }
	}

	/// Extend the message index window by `count`. Returns the latest used message index.
	/// Panics if extending over capacity, similarly to `RingBuffer`.
	pub fn extend(&mut self, count: u64) -> ParaMessageIndex {
		if self.size() > 0 {
			let free_count =
				self.state.first_message_idx.wrapping_sub(self.state.free_message_idx).0;

			if free_count < count {
				panic!("The end of the world is upon us");
			}
		}

		self.state.free_message_idx = self.state.free_message_idx.wrapping_add(count.into());
		ParaMessageIndex {
			para_id: self.para_id,
			message_idx: self.state.free_message_idx.wrapping_dec(),
		}
	}

	/// Advanced the window start by `count` elements.  Returns the index of the first element in queue
	/// or `None` if the queue is empty after the operation.
	pub fn prune(&mut self, count: u64) -> Option<ParaMessageIndex> {
		let to_prune = sp_std::cmp::min(self.size(), count);
		self.state.first_message_idx = self.state.first_message_idx.wrapping_add(to_prune.into());
		if self.state.first_message_idx == self.state.free_message_idx {
			None
		} else {
			Some(ParaMessageIndex { para_id: self.para_id, message_idx: self.state.first_message_idx })
		}
	}

	/// Returns the size of the message window.
	pub fn size(&self) -> u64 {
		self.state.free_message_idx.wrapping_sub(self.state.first_message_idx).into()
	}

	/// Returns the first message index, `None` if window is empty.
	pub fn first(&self) -> Option<ParaMessageIndex> {
		if self.size() > 0 {
			Some(ParaMessageIndex { para_id: self.para_id, message_idx: self.state.first_message_idx })
		} else {
			None
		}
	}

	/// Returns the first free message index.
	pub fn first_free(&self) -> ParaMessageIndex {
		ParaMessageIndex { para_id: self.para_id, message_idx: self.state.free_message_idx }
	}

	/// Returns the wrapped state.
	pub fn into_inner(self) -> MessageWindowState {
		self.state
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ringbuf_extend() {
		let mut rb = RingBuffer::with_state(RingBufferState::default(), 0.into());
		assert!(rb.front().is_none());
		assert!(rb.last_used().is_none());

		let mut page = rb.extend();
		assert_eq!(page.page_idx, 0.into());
		page = rb.extend();
		assert_eq!(page.page_idx, 1.into());
		assert_eq!(rb.size(), 2);

		assert_eq!(rb.front().unwrap().page_idx, 0.into());
		assert_eq!(rb.last_used().unwrap().page_idx, 1.into());
	}

	#[test]
	#[should_panic]
	fn ringbuf_extend_over_capacity() {
		// This ringbuf will have 2 free pages.
		let head = 100.into();
		let tail = 98.into();
		let mut rb = RingBuffer::with_state(
			RingBufferState { head_page_idx: head, tail_page_idx: tail },
			0.into(),
		);

		rb.extend();
		rb.extend();
		// This should panic!
		rb.extend();
	}

	#[test]
	fn ringbuf_extend_loop_then_prune() {
		let mut rb = RingBuffer::with_state(RingBufferState::default(), 0.into());
		assert!(rb.front().is_none());
		assert!(rb.last_used().is_none());

		for _ in 0..1024 {
			rb.extend();
		}

		assert_eq!(rb.size(), 1024);
		assert_eq!(rb.front().unwrap().page_idx, 0.into());
		assert_eq!(rb.last_used().unwrap().page_idx, 1023.into());

		// Test we can prune 0 pages.
		rb.prune(0);
		assert_eq!(rb.size(), 1024);
		assert_eq!(rb.front().unwrap().page_idx, 0.into());
		assert_eq!(rb.last_used().unwrap().page_idx, 1023.into());

		// Test we can prune 1 page.
		rb.prune(1);
		assert_eq!(rb.size(), 1023);
		assert_eq!(rb.front().unwrap().page_idx, 1.into());
		assert_eq!(rb.last_used().unwrap().page_idx, 1023.into());

		// Test we can prune all pages.
		rb.prune(99999);
		assert_eq!(rb.size(), 0);
		assert_eq!(rb.front(), None);
		assert_eq!(rb.last_used(), None);
	}

	#[test]
	fn ringbuf_extend_loop_then_pop_until_empty() {
		let mut rb = RingBuffer::with_state(RingBufferState::default(), 0.into());
		assert!(rb.front().is_none());
		assert!(rb.last_used().is_none());

		for _ in 0..1024 {
			rb.extend();
		}

		assert_eq!(rb.size(), 1024);
		assert_eq!(rb.front().unwrap().page_idx, 0.into());
		assert_eq!(rb.last_used().unwrap().page_idx, 1023.into());

		let mut idx: WrappingIndex<PageIndex> = 0u64.into();

		while rb.size() > 0 {
			let page = rb.pop_front().unwrap();
			assert_eq!(idx, page.page_idx);
			idx = idx.wrapping_inc();
		}

		assert_eq!(rb.size(), 0);
		assert_eq!(rb.front(), None);
		assert_eq!(rb.last_used(), None);
	}

	#[test]
	fn ringbuf_extend_loop_then_iterate_wrap_around() {
		let mut head: WrappingIndex<PageIndex> = 0u64.into();
		head = head.wrapping_sub(512.into());
		let mut rb = RingBuffer::with_state(
			RingBufferState { head_page_idx: head, tail_page_idx: head },
			0.into(),
		);
		assert!(rb.front().is_none());
		assert!(rb.last_used().is_none());

		for _ in 0..1024 {
			rb.extend();
		}

		assert_eq!(rb.size(), 1024);
		assert_eq!(rb.front().unwrap().page_idx, head);
		assert_eq!(rb.last_used().unwrap().page_idx, 511.into());

		let mut idx = head;

		for page in rb {
			assert_eq!(idx, page.page_idx);
			idx = idx.wrapping_inc();
		}
	}

	#[test]
	fn message_window_extend() {
		let mut window = MessageWindow::with_state(MessageWindowState::default(), 0.into());
		assert_eq!(window.size(), 0);
		assert_eq!(window.first(), None);
		assert_eq!(window.first_free().message_idx, 0.into());

		let msg_idx = window.extend(1).message_idx;
		assert_eq!(msg_idx, 0.into());
	}

	#[test]
	#[should_panic]
	fn message_window_extend_over_capacity() {
		let mut window = MessageWindow::with_state(
			MessageWindowState { first_message_idx: 10.into(), free_message_idx: 2.into() },
			0.into(),
		);

		// This should panic!
		window.extend(10);
	}

	#[test]
	fn message_window_extend_then_prune() {
		let mut window = MessageWindow::with_state(MessageWindowState::default(), 0.into());
		assert_eq!(window.size(), 0);
		assert_eq!(window.first(), None);
		assert_eq!(window.first_free().message_idx, 0.into());

		window.extend(1024);

		for _ in 0..1024 {
			window.extend(2);
		}

		window.extend(1024);

		assert_eq!(window.size(), 4096);
		assert_eq!(window.first().unwrap().message_idx, 0.into());
		assert_eq!(window.first_free().message_idx, 4096.into());

		// Test we can prune 0 messages.
		window.prune(0);
		assert_eq!(window.size(), 4096);
		assert_eq!(window.first().unwrap().message_idx, 0.into());
		assert_eq!(window.first_free().message_idx, 4096.into());

		// Test we can prune 1 message.
		window.prune(1);
		assert_eq!(window.size(), 4095);
		assert_eq!(window.first().unwrap().message_idx, 1.into());
		assert_eq!(window.first_free().message_idx, 4096.into());

		// Test we can prune all messages.
		window.prune(99999);
		assert_eq!(window.size(), 0);
		assert_eq!(window.first(), None);
		assert_eq!(window.first_free().message_idx, 4096.into());
	}
}
