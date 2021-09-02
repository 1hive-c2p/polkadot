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

use crate::{Assets, PhantomData};
use frame_support::{dispatch::GetDispatchInfo, weights::Weight};
use parity_scale_codec::Decode;
use sp_runtime::traits::Saturating;
use sp_std::result::Result;
use xcm::latest::prelude::*;

/// Determine the weight of an XCM message.
pub trait WeightBounds<Call> {
	/// Return the maximum amount of weight that an attempted execution of this message could
	/// consume.
	fn weight(message: &mut Xcm<Call>) -> Result<Weight, ()>;

	/// Return the maximum amount of weight that an attempted execution of this instruction could
	/// consume.
	fn instr_weight(instruction: &Instruction<Call>) -> Result<Weight, ()>;
}

/// A means of getting approximate weight consumption for a given destination message executor and a
/// message.
pub trait UniversalWeigher {
	/// Get the upper limit of weight required for `dest` to execute `message`.
	fn weigh(dest: MultiLocation, message: Xcm<()>) -> Result<Weight, ()>;
}

/// Charge for weight in order to execute XCM.
///
/// A `WeightTrader` may also be put into a tuple, in which case the default behavior of
/// `buy_weight` and `refund_weight` would be to attempt to call each tuple element's own
/// implementation of these two functions, in the order of which they appear in the tuple,
/// returning early when a successful result is returned.
pub trait WeightTrader: Sized {
	/// Create a new trader instance.
	fn new() -> Self;

	/// Purchase execution weight credit in return for up to a given `fee`. If less of the fee is required
	/// then the surplus is returned. If the `fee` cannot be used to pay for the `weight`, then an error is
	/// returned.
	fn buy_weight(&mut self, weight: Weight, payment: Assets) -> Result<Assets, XcmError>;

	/// Attempt a refund of `weight` into some asset. The caller does not guarantee that the weight was
	/// purchased using `buy_weight`.
	///
	/// Default implementation refunds nothing.
	fn refund_weight(&mut self, _weight: Weight) -> Option<MultiAsset> {
		None
	}
}

#[impl_trait_for_tuples::impl_for_tuples(30)]
impl WeightTrader for Tuple {
	fn new() -> Self {
		for_tuples!( ( #( Tuple::new() ),* ) )
	}

	fn buy_weight(&mut self, weight: Weight, payment: Assets) -> Result<Assets, XcmError> {
		let mut last_error = None;
		for_tuples!( #(
			match Tuple.buy_weight(weight, payment.clone()) {
				Ok(assets) => return Ok(assets),
				Err(e) => { last_error = Some(e) }
			}
		)* );
		let last_error = last_error.unwrap_or(XcmError::TooExpensive);
		log::trace!(target: "xcm::buy_weight", "last_error: {:?}", last_error);
		Err(last_error)
	}

	fn refund_weight(&mut self, weight: Weight) -> Option<MultiAsset> {
		for_tuples!( #(
			if let Some(asset) = Tuple.refund_weight(weight) {
				return Some(asset);
			}
		)* );
		None
	}
}

struct FinalXcmWeight<W, C>(PhantomData<(W, C)>);
impl<W, C> WeightBounds<C> for FinalXcmWeight<W, C>
where
	W: XcmWeightInfo<C>,
	C: Decode + GetDispatchInfo,
	Xcm<C>: GetWeight<W>,
	Order<C>: GetWeight<W>,
{
	fn shallow(message: &mut Xcm<C>) -> Result<Weight, ()> {
		let weight = match message {
			Xcm::RelayedFrom { ref mut message, .. } => {
				let relay_message_weight = Self::shallow(message.as_mut())?;
				message.weight().saturating_add(relay_message_weight)
			},
			// These XCM
			Xcm::WithdrawAsset { effects, .. } |
			Xcm::ReserveAssetDeposited { effects, .. } |
			Xcm::ReceiveTeleportedAsset { effects, .. } => {
				let mut extra = 0;
				for order in effects.iter_mut() {
					extra.saturating_accrue(Self::shallow_order(order)?);
				}
				extra.saturating_accrue(message.weight());
				extra
			},
			// The shallow weight of `Transact` is the full weight of the message, thus there is no
			// deeper weight.
			Xcm::Transact { call, .. } => {
				let call_weight = call.ensure_decoded()?.get_dispatch_info().weight;
				message.weight().saturating_add(call_weight)
			},
			// These
			Xcm::QueryResponse { .. } |
			Xcm::TransferAsset { .. } |
			Xcm::TransferReserveAsset { .. } |
			Xcm::HrmpNewChannelOpenRequest { .. } |
			Xcm::HrmpChannelAccepted { .. } |
			Xcm::HrmpChannelClosing { .. } => message.weight(),
		};

		Ok(weight)
	}

	fn deep(message: &mut Xcm<C>) -> Result<Weight, ()> {
		let weight = match message {
			// `RelayFrom` needs to account for the deep weight of the internal message.
			Xcm::RelayedFrom { ref mut message, .. } => Self::deep(message.as_mut())?,
			// These XCM have internal effects which are not accounted for in the `shallow` weight.
			Xcm::WithdrawAsset { effects, .. } |
			Xcm::ReserveAssetDeposited { effects, .. } |
			Xcm::ReceiveTeleportedAsset { effects, .. } => {
				let mut extra: Weight = 0;
				for order in effects.iter_mut() {
					extra.saturating_accrue(Self::deep_order(order)?);
				}
				extra
			},
			// These XCM do not have any deeper weight.
			Xcm::Transact { .. } |
			Xcm::QueryResponse { .. } |
			Xcm::TransferAsset { .. } |
			Xcm::TransferReserveAsset { .. } |
			Xcm::HrmpNewChannelOpenRequest { .. } |
			Xcm::HrmpChannelAccepted { .. } |
			Xcm::HrmpChannelClosing { .. } => 0,
		};

		Ok(weight)
	}
}

impl<W, C> FinalXcmWeight<W, C>
where
	W: XcmWeightInfo<C>,
	C: Decode + GetDispatchInfo,
	Xcm<C>: GetWeight<W>,
	Order<C>: GetWeight<W>,
{
	fn shallow_order(order: &mut Order<C>) -> Result<Weight, ()> {
		Ok(match order {
			Order::BuyExecution { fees, weight, debt, halt_on_error, instructions } => {
				// On success, execution of this will result in more weight being consumed but
				// we don't count it here since this is only the *shallow*, non-negotiable weight
				// spend and doesn't count weight placed behind a `BuyExecution` since it will not
				// be definitely consumed from any existing weight credit if execution of the message
				// is attempted.
				W::order_buy_execution(fees, weight, debt, halt_on_error, instructions)
			},
			_ => 0, // TODO check
		})
	}
	fn deep_order(order: &mut Order<C>) -> Result<Weight, ()> {
		Ok(match order {
			Order::BuyExecution { instructions, .. } => {
				let mut extra = 0;
				for instruction in instructions.iter_mut() {
					extra.saturating_accrue(
						Self::shallow(instruction)?.saturating_add(Self::deep(instruction)?),
					);
				}
				extra
			},
			_ => 0,
		})
	}
}
