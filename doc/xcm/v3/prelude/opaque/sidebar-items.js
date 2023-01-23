window.SIDEBAR_ITEMS = {"enum":[["AssetId","Classification of an asset being concrete or abstract."],["AssetInstance","A general identifier for an instance of a non-fungible asset class."],["BodyId","An identifier of a pluralistic body."],["BodyPart","A part of a pluralistic body."],["Fungibility","Classification of whether an asset is fungible or not, along with a mandatory amount or instance."],["Junction","A single item in a path to describe the relative location of a consensus system."],["Junctions","Non-parent junctions that can be constructed, up to the length of 8. This specific `Junctions` implementation uses a Rust `enum` in order to make pattern matching easier."],["MultiAssetFilter","`MultiAsset` collection, defined either by a number of `MultiAssets` or a single wildcard."],["NetworkId","A global identifier of a data structure existing within consensus."],["Outcome","Outcome of an XCM execution."],["SendError","Error result value when attempting to send an XCM message."],["WildFungibility","Classification of whether an asset is fungible or not."],["WildMultiAsset","A wildcard representing a set of assets."],["XcmError","Error codes used in XCM. The first errors codes have explicit indices and are part of the XCM format. Those trailing are merely part of the XCM implementation; there is no expectation that they will retain the same index over time."]],"fn":[["send_xcm","Convenience function for using a `SendXcm` implementation. Just interprets the `dest` and wraps both in `Some` before passing them as as mutable references into `T::send_xcm`."],["validate_send","Convenience function for using a `SendXcm` implementation. Just interprets the `dest` and wraps both in `Some` before passing them as as mutable references into `T::send_xcm`."]],"struct":[["Ancestor","A unit struct which can be converted into a `MultiLocation` of the inner `parents` value."],["AncestorThen","A unit struct which can be converted into a `MultiLocation` of the inner `parents` value and the inner interior."],["MultiAsset","Either an amount of a single fungible asset, or a single well-identified non-fungible asset."],["MultiAssets","A `Vec` of `MultiAsset`s."],["MultiLocation","A relative path between state-bearing consensus systems."],["Parent","A unit struct which can be converted into a `MultiLocation` of `parents` value 1."],["ParentThen","A tuple struct which can be converted into a `MultiLocation` of `parents` value 1 with the inner interior."]],"trait":[["ExecuteXcm","Type of XCM message executor."],["PreparedMessage",""],["SendXcm","Utility for sending an XCM message to a given location."],["Unwrappable",""]],"type":[["InteriorMultiLocation","A relative location which is constrained to be an interior location of the context."],["SendResult","Result value when attempting to send an XCM message."],["XcmHash","A hash type for identifying messages."],["XcmResult",""]]};