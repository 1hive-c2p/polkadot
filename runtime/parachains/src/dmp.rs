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

//! This is a low level runtime component that manages the downward message queue for each
//! parachain. Messages are stored on the relay chain until they are processed by destination
//! parachains.
//!
//! The methods exposed here allow extending, reading and pruning of the message queue.
//!
//! Message queue storage format:
//! - The messages are queued in a ring buffer. There is a 1:1 mapping between a queue and
//! each parachain.
//! - The ring buffer stores pages of up to `QUEUE_PAGE_CAPACITY` messages each.
//!
//! When sending messages, higher level code calls the `queue_downward_message` method which only fails
//! if the message size is higher than what the configuration defines in `max_downward_message_size`.
//! For every message sent we assign a sequential index and we store it for the first and last messages
//! in the queue.
//!
//! When a parachain consumes messages, they'll need a way to ensure the messages, or their ordering
//! were not altered in any way. A message queue chain(MQC) solves this as long as the last processed
//! head hash is available to the parachain. After sequentially hashing a subset of messages from
//! the message queue (typically up to a certain weight), the parachain should arrive at the same MQC
//! head as the one provided by the relay chain.
//! This is implemented as a mapping between the message index and the MQC head for any given para.
//! That being said, parachains runtimes should also track the message indices to access the MQC storage
//! proof.

use crate::{
	configuration::{self, HostConfiguration},
	initializer,
};

use frame_support::{pallet_prelude::*, weights::Weight};
use primitives::v2::{
	DmqContentsBounds, DownwardMessage, Hash, Id as ParaId, InboundDownwardMessage,
};
use sp_runtime::traits::{BlakeTwo256, Hash as HashT};
use sp_std::{fmt, prelude::*};
use xcm::latest::SendError;

pub use pallet::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
use polkadot_parachain::primitives::{MessageIndex, PageIndex, WrappingIndex};

pub mod migration;
pub mod ringbuf;
pub use ringbuf::*;

/// The state of the queue split in two sub-states, the ring bufer and the message window.
///
/// Invariants - see `RingBufferState` and `MessageWindowState`.
#[derive(Encode, Decode, Default, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo)]
pub struct QueueState {
	pub ring_buffer_state: RingBufferState,
	pub message_window_state: MessageWindowState,
}

/// An error sending a downward message.
#[derive(Debug)]
pub enum QueueDownwardMessageError {
	/// The message being sent exceeds the configured max message size.
	ExceedsMaxMessageSize,
}

impl From<QueueDownwardMessageError> for SendError {
	fn from(err: QueueDownwardMessageError) -> Self {
		match err {
			QueueDownwardMessageError::ExceedsMaxMessageSize => SendError::ExceedsMaxMessageSize,
		}
	}
}

/// An error returned by [`check_processed_downward_messages`] that indicates an acceptance check
/// didn't pass.
pub enum ProcessedDownwardMessagesAcceptanceErr {
	/// If there are pending messages then `processed_downward_messages` should be at least 1,
	AdvancementRule,
	/// `processed_downward_messages` should not be greater than the number of pending messages.
	Underflow { processed_downward_messages: u32, dmq_length: u32 },
}

impl fmt::Debug for ProcessedDownwardMessagesAcceptanceErr {
	fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
		use ProcessedDownwardMessagesAcceptanceErr::*;
		match *self {
			AdvancementRule =>
				write!(fmt, "DMQ is not empty, but processed_downward_messages is 0",),
			Underflow { processed_downward_messages, dmq_length } => write!(
				fmt,
				"processed_downward_messages = {}, but dmq_length is only {}",
				processed_downward_messages, dmq_length,
			),
		}
	}
}

/// To readjust the memory footprint when sending or receiving messages we will split
/// the queue in pages of `QUEUE_PAGE_CAPACITY` capacity. Tuning this constant allows
/// to control how we trade off the overhead per stored message vs memory footprint of individual
/// messages read. The pages are part of ring buffer per para and we keep track of the head and tail page index.
///
///
/// Defines the queue page capacity. Storage key count is inversely correlated to page capacity.
/// When requesting pages of messages, we must make sure that this value is low enough so that all
/// messages in the 1 page can fit in the runtime memory. This value was arbitrarily chosen `wrt` the
/// Kusama configuration value of `maxDownwardMessageSize: 51,200 bytes`.
pub const QUEUE_PAGE_CAPACITY: u32 = 32;

#[frame_support::pallet]
pub mod pallet {
	use super::*;

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	#[pallet::storage_version(migration::STORAGE_VERSION)]
	#[pallet::without_storage_info]
	pub struct Pallet<T>(_);

	#[pallet::config]
	pub trait Config: frame_system::Config + configuration::Config {
		/// Maximum number of messages per page.
		type DmpPageCapacity: Get<u32>;
	}

	/// A mapping between parachains and their message queue state.
	#[pallet::storage]
	#[pallet::getter(fn dmp_queue_state)]
	pub(super) type DownwardMessageQueueState<T: Config> =
		StorageMap<_, Twox64Concat, ParaId, QueueState, ValueQuery>;

	/// A mapping between the queue pages of a parachain and the messages stored in it.
	///
	/// Invariants:
	/// - Downward message count equals message window size.
	#[pallet::storage]
	pub(crate) type DownwardMessageQueuePages<T: Config> = StorageMap<
		_,
		Twox64Concat,
		QueuePageIndex,
		BoundedVec<InboundDownwardMessage<T::BlockNumber>, T::DmpPageCapacity>,
		ValueQuery,
	>;

	/// A mapping that stores the downward message queue MQC head for each para.
	///
	/// Each link in this chain has a form:
	/// `(prev_head, B, H(M))`, where
	/// - `prev_head`: is the previous head hash or zero if none.
	/// - `B`: is the relay-chain block number in which a message was appended.
	/// - `H(M)`: is the hash of the message being appended.
	#[pallet::storage]
	pub(crate) type DownwardMessageQueueHeads<T: Config> =
		StorageMap<_, Twox64Concat, ParaId, Hash, ValueQuery>;

	/// A mapping between a message and the corresponding MQC head hash.
	///
	/// Invariants:
	/// - the storage value is valid for any `MessageIndex` in the current message window
	#[pallet::storage]
	pub(crate) type DownwardMessageQueueHeadsById<T: Config> =
		StorageMap<_, Twox64Concat, ParaMessageIndex, Hash, ValueQuery>;

	#[pallet::call]
	impl<T: Config> Pallet<T> {}
}

/// Routines and getters related to downward message passing.
impl<T: Config> Pallet<T> {
	/// Block initialization logic, called by initializer.
	pub(crate) fn initializer_initialize(_now: T::BlockNumber) -> Weight {
		Weight::zero()
	}

	/// Block finalization logic, called by initializer.
	pub(crate) fn initializer_finalize() {}

	/// Called by the initializer to note that a new session has started.
	pub(crate) fn initializer_on_new_session(
		_notification: &initializer::SessionChangeNotification<T::BlockNumber>,
		outgoing_paras: &[ParaId],
	) {
		Self::perform_outgoing_para_cleanup(outgoing_paras);
	}

	/// Iterate over all paras that were noted for offboarding and remove all the data
	/// associated with them.
	fn perform_outgoing_para_cleanup(outgoing: &[ParaId]) {
		for outgoing_para in outgoing {
			Self::clean_dmp_after_outgoing(outgoing_para);
		}
	}

	pub(crate) fn update_state(para: &ParaId, new_state: QueueState) -> Weight {
		<Self as Store>::DownwardMessageQueueState::mutate(para, |state| {
			*state = new_state;
		});

		T::DbWeight::get().reads_writes(1, 1)
	}

	/// Remove all relevant storage items for an outgoing parachain.
	fn clean_dmp_after_outgoing(outgoing_para: &ParaId) {
		let state = Self::dmp_queue_state(outgoing_para);

		for page_idx in RingBuffer::with_state(state.ring_buffer_state, *outgoing_para) {
			<Self as Store>::DownwardMessageQueuePages::remove(page_idx);
		}

		<Self as Store>::DownwardMessageQueueHeads::remove(outgoing_para);
	}

	/// Enqueue a downward message to a specific recipient para.
	///
	/// When encoded, the message should not exceed the `config.max_downward_message_size`.
	/// Otherwise, the message won't be sent and `Err` will be returned.
	///
	/// It is possible to send a downward message to a non-existent para. That, however, would lead
	/// to a dangling storage. If the caller cannot statically prove that the recipient exists
	/// then the caller should perform a runtime check.
	pub fn queue_downward_message(
		config: &HostConfiguration<T::BlockNumber>,
		para: ParaId,
		msg: DownwardMessage,
	) -> Result<Weight, QueueDownwardMessageError> {
		// Check if message is oversized.
		let serialized_len = msg.len() as u32;
		if serialized_len > config.max_downward_message_size {
			return Err(QueueDownwardMessageError::ExceedsMaxMessageSize)
		}

		let mut weight = Weight::zero();
		let QueueState { ring_buffer_state, message_window_state } = Self::dmp_queue_state(para);
		weight = weight.saturating_add(T::DbWeight::get().reads_writes(1, 0));

		let mut ring_buf = RingBuffer::with_state(ring_buffer_state, para);
		let mut message_window = MessageWindow::with_state(message_window_state, para);

		let inbound =
			InboundDownwardMessage { msg, sent_at: <frame_system::Pallet<T>>::block_number() };
		// Obtain the new link in the MQC and update the head.
		<Self as Store>::DownwardMessageQueueHeads::mutate(para, |head| {
			let new_head =
				BlakeTwo256::hash_of(&(*head, inbound.sent_at, T::Hashing::hash_of(&inbound.msg)));
			*head = new_head;

			// Extend the message window by `1` message get it's index.
			let new_message_idx = message_window.extend(1);

			// Update the head for the current message.
			<Self as Store>::DownwardMessageQueueHeadsById::mutate(new_message_idx, |head| {
				*head = new_head
			});
		});

		// Get a new page.
		let mut page_idx = ring_buf.last_used().unwrap_or_else(|| ring_buf.extend());
		let mut page = <Self as Store>::DownwardMessageQueuePages::get(&page_idx);
		weight = weight.saturating_add(T::DbWeight::get().reads_writes(1, 0));

		// Insert message in the tail queue page.
		if page.try_push(inbound.clone()).is_ok() {
			<Self as Store>::DownwardMessageQueuePages::insert(&page_idx, &page);
		} else {
			page_idx = ring_buf.extend();
			let page = BoundedVec::<_, T::DmpPageCapacity>::try_from(vec![inbound])
				.expect("one message always fits");
			<Self as Store>::DownwardMessageQueuePages::insert(&page_idx, page);
		}

		// For the above mutate.
		weight = weight.saturating_add(T::DbWeight::get().reads_writes(3, 3));

		let ring_buffer_state = ring_buf.into_inner();
		let message_window_state = message_window.into_inner();
		weight = weight.saturating_add(Self::update_state(
			&para,
			QueueState { ring_buffer_state, message_window_state },
		));

		Ok(weight)
	}

	/// Checks if the number of processed downward messages is valid.
	pub(crate) fn check_processed_downward_messages(
		para: ParaId,
		processed_downward_messages: u32,
	) -> Result<(), ProcessedDownwardMessagesAcceptanceErr> {
		let dmq_length = Self::dmq_length(para);

		if dmq_length > 0 && processed_downward_messages == 0 {
			return Err(ProcessedDownwardMessagesAcceptanceErr::AdvancementRule)
		}
		if dmq_length < processed_downward_messages {
			return Err(ProcessedDownwardMessagesAcceptanceErr::Underflow {
				processed_downward_messages,
				dmq_length,
			})
		}

		Ok(())
	}

	/// MQC head key generator. Useful for pruning entries. Returns `count` MQC head mapping keys
	/// of the messages starting at index `start` for a given parachain.
	///
	/// Caller must ensure the indices return are valid in the context of the `MessageWindow`.
	#[cfg(test)]
	fn mqc_head_key_range(
		para: ParaId,
		start: WrappingIndex<MessageIndex>,
		count: u64,
	) -> Vec<ParaMessageIndex> {
		let mut keys = Vec::new();
		let mut idx = start;
		while idx != start.wrapping_add(count.into()) {
			keys.push(ParaMessageIndex { para_id: para, message_idx: idx });
			idx = idx.wrapping_inc();
		}

		keys
	}

	/// Prunes the specified number of messages from the downward message queue of the given para.
	pub(crate) fn prune_dmq(para: ParaId, processed_downward_messages: u32) -> Weight {
		let QueueState { ring_buffer_state, message_window_state } = Self::dmp_queue_state(para);
		let mut message_window = MessageWindow::with_state(message_window_state, para);
		let queue_length = message_window.size();
		let mut total_weight = T::DbWeight::get().reads_writes(1, 0);

		// Bail out early if the queue is empty.
		if queue_length == 0 {
			return total_weight
		}

		// A call to [`check_processed_downward_messages`] will check if `processed_downward_messages`
		// is greater than total messages in queue. so we don't need to check that again here.

		if processed_downward_messages == 0 {
			// This should never happen in practice, because of the advancement rule - parachains must
			// process one message at least per para block.
			log::warn!(
				target: "runtime::dmp",
				"Dmq pruning called with no processed messages",
			);
			debug_assert!(false);
			return total_weight
		}

		let mut ring_buf = RingBuffer::with_state(ring_buffer_state, para);
		let mut messages_to_prune = processed_downward_messages as u64;

		let first_mqc_key_to_remove =
			message_window.first().expect("queue is not empty").message_idx;
		let mut pruned_message_count = 0;

		while messages_to_prune > 0 {
			if let Some(first_used_page) = ring_buf.front() {
				let mut page = <Self as Store>::DownwardMessageQueuePages::get(&first_used_page);
				let messages_in_page = page.len() as u64;

				if messages_to_prune >= messages_in_page {
					messages_to_prune = messages_to_prune.saturating_sub(messages_in_page);
					message_window.prune(messages_in_page);
					// Update storage - remove page.
					<Self as Store>::DownwardMessageQueuePages::remove(&first_used_page);
					total_weight += T::DbWeight::get().reads_writes(0, 1);

					// Free the ring buffer page.
					ring_buf.pop_front();

					pruned_message_count += messages_in_page;
				} else {
					message_window.prune(messages_to_prune);
					let mut dumb_vec: Vec<_> = page.into();
					page = BoundedVec::<_, T::DmpPageCapacity>::try_from(
						dumb_vec.split_off(messages_to_prune as usize),
					)
					.expect("a subset is always bounded; qed");

					pruned_message_count += messages_to_prune;

					// Update storage - write back remaining messages.
					<Self as Store>::DownwardMessageQueuePages::insert(&first_used_page, page);

					// Break loop.
					messages_to_prune = 0;
				}

				// Add mutate weight. Removal happens later.
				total_weight += T::DbWeight::get().reads_writes(1, 1);
			} else {
				// Queue is empty.
				break
			}
		}

		total_weight += T::DbWeight::get().reads_writes(0, pruned_message_count);

		let mut message_idx = first_mqc_key_to_remove;
		while message_idx != first_mqc_key_to_remove.wrapping_add(pruned_message_count.into()) {
			<Self as Store>::DownwardMessageQueueHeadsById::remove(ParaMessageIndex {
				para_id: para,
				message_idx,
			});
			message_idx = message_idx.wrapping_inc();
		}

		let ring_buffer_state = ring_buf.into_inner();
		let message_window_state = message_window.into_inner();
		total_weight = total_weight.saturating_add(Self::update_state(
			&para,
			QueueState { ring_buffer_state, message_window_state },
		));
		total_weight += T::DbWeight::get().reads_writes(0, 1);

		total_weight
	}

	/// Returns the Head of Message Queue Chain for the given para or `None` if there is none
	/// associated with it.
	#[cfg(test)]
	fn dmq_mqc_head(para: ParaId) -> Hash {
		<Self as Store>::DownwardMessageQueueHeads::get(&para)
	}

	#[cfg(test)]
	fn dmq_mqc_head_for_message(para_id: ParaId, message_idx: WrappingIndex<MessageIndex>) -> Hash {
		<Self as Store>::DownwardMessageQueueHeadsById::get(&ParaMessageIndex {
			para_id,
			message_idx,
		})
	}

	/// Returns the number of pending downward messages addressed to the given para.
	///
	/// Returns 0 if the para doesn't have an associated downward message queue.
	pub(crate) fn dmq_length(para: ParaId) -> u32 {
		let state = Self::dmp_queue_state(para);
		MessageWindow::with_state(state.message_window_state, para).size() as u32
	}

	/// Returns all the messages from the DMP queue.
	/// Deprecated API. Please use `dmq_contents_bounded`.
	pub(crate) fn dmq_contents(recipient: ParaId) -> Vec<InboundDownwardMessage<T::BlockNumber>> {
		let state = Self::dmp_queue_state(recipient);
		Self::dmq_contents_bounded(
			recipient,
			DmqContentsBounds {
				start_page_index: 0,
				page_count: RingBuffer::with_state(state.ring_buffer_state, recipient).size()
					as u32,
			},
		)
	}

	/// Get a subset of inbound messages from the downward message queue of a parachain.
	///
	/// Returns a `vec` containing the messages from the first `bounds.page_count` pages, starting from a `0` based
	/// page index specified by `bounds.start_page_index` with `0` being the first used page of the queue. A page
	/// can hold up to `QUEUE_PAGE_CAPACITY` messages. (please see the runtime `dmp` implementation).
	///
	/// Only the outer pages of the queue can have less than maximum messages because insertion and
	/// pruning work with individual messages.
	///
	/// The result will be an empty vector if `bounds.page_count` is 0, the para doesn't exist, it's queue is empty
	/// or `bounds.start_page_index` is greater than the last used page in the queue. If the queue is not empty, the method
	/// is guaranteed to return at least 1 message and up to `bounds.page_count`*`QUEUE_PAGE_CAPACITY` messages.
	pub(crate) fn dmq_contents_bounded(
		recipient: ParaId,
		bounds: DmqContentsBounds,
	) -> Vec<InboundDownwardMessage<T::BlockNumber>> {
		let state = Self::dmp_queue_state(recipient);
		let mut ring_buf = RingBuffer::with_state(state.ring_buffer_state, recipient);

		// Skip first `bounds.start_page_index` pages.
		ring_buf.prune(bounds.start_page_index);

		let mut result =
			Vec::with_capacity((bounds.page_count.saturating_mul(QUEUE_PAGE_CAPACITY)) as usize);

		let mut pages_fetched = 0;

		for page_idx in ring_buf {
			if bounds.page_count == pages_fetched {
				break
			}
			result.extend(<Self as Store>::DownwardMessageQueuePages::get(page_idx));
			pages_fetched += 1;
		}

		result
	}

	#[cfg(test)]
	/// Test utility for generating a sequence of page indices.
	fn page_key_range(
		para_id: ParaId,
		start: WrappingIndex<PageIndex>,
		count: u64,
	) -> Vec<QueuePageIndex> {
		let mut keys = Vec::new();
		let mut page_idx = start;
		while page_idx != start.wrapping_add(count.into()) {
			keys.push(QueuePageIndex { para_id, page_idx });
			page_idx = page_idx.wrapping_inc();
		}

		keys
	}

	/// A critical utility for testing: it checks the storage invariants. Should be called after each storage update.
	#[cfg(test)]
	fn assert_storage_consistency_exhaustive(last_pruned_mqc_head: Option<Hash>) {
		let all_queue_states = <Self as Store>::DownwardMessageQueueState::iter()
			.collect::<sp_std::collections::btree_map::BTreeMap<ParaId, QueueState>>(
		);

		for (para_id, state) in all_queue_states.into_iter() {
			let ring_buf = RingBuffer::with_state(state.ring_buffer_state, para_id);
			let window = MessageWindow::with_state(state.message_window_state, para_id);

			// Fetch all messages for this para.
			let messages_in_pages = ring_buf
				.into_iter()
				.map(|page_idx| <Self as Store>::DownwardMessageQueuePages::get(page_idx))
				.flatten()
				.collect::<Vec<_>>();

			// Ensure 1:1 mapping of messages to message indices as defined by the message window.
			assert_eq!(messages_in_pages.len() as u64, window.size());

			// If ring not empty, ensure we have MQC heads properly set.
			if ring_buf.size() > 0 {
				let mut mqc = last_pruned_mqc_head.unwrap_or(Hash::zero());
				let mqc_head = &mut mqc;
				let computed_mqc_heads = messages_in_pages
					.into_iter()
					.map(|message| {
						let new_head = BlakeTwo256::hash_of(&(
							*mqc_head,
							message.sent_at,
							T::Hashing::hash_of(&message.msg),
						));
						*mqc_head = new_head;
						new_head
					})
					.collect::<Vec<_>>();

				let all_mqc_heads = Self::mqc_head_key_range(
					para_id,
					// Guaranteed to not panic, see invariants.
					window.first().unwrap().message_idx,
					window.size(),
				)
				.into_iter()
				.filter_map(|message_idx| {
					match <Self as Store>::DownwardMessageQueueHeadsById::try_get(message_idx) {
						Ok(value) => Some(value),
						Err(()) => None,
					}
				})
				.collect::<Vec<_>>();

				assert_eq!(all_mqc_heads, computed_mqc_heads);
			}

			// Ensure pruning keeps things tidy - mqc heads per message and pages are freed.
			// Checking entire ringbuf would take ages, instead we check 4096 keys outside the window
			let mut mqc_keys_to_check = Vec::new();

			mqc_keys_to_check.extend(Self::mqc_head_key_range(
				para_id,
				window
					.first()
					.unwrap_or(window.first_free())
					.message_idx
					.wrapping_sub(4097.into()),
				4096,
			));

			mqc_keys_to_check.extend(Self::mqc_head_key_range(
				para_id,
				window.first_free().message_idx,
				4096,
			));

			for message_idx in mqc_keys_to_check {
				<Self as Store>::DownwardMessageQueueHeadsById::try_get(message_idx).unwrap_err();
			}

			// Now check if we have dangling pages.
			let mut page_keys_to_check = Vec::new();

			page_keys_to_check.extend(Self::page_key_range(
				para_id,
				ring_buf
					.front()
					.unwrap_or(ring_buf.first_unused())
					.page_idx
					.wrapping_sub(4097.into()),
				4096,
			));

			page_keys_to_check.extend(Self::page_key_range(
				para_id,
				ring_buf.first_unused().page_idx,
				4096,
			));

			for page_idx in page_keys_to_check {
				<Self as Store>::DownwardMessageQueuePages::try_get(page_idx).unwrap_err();
			}
		}
	}
}
