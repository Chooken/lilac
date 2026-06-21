const sema = @import("../sema.zig");
const untyped = @import("../untyped.zig");
const typed = @import("../typed.zig");

const idCollectionPass = @import("analysis/id_collection_pass.zig");
const typeDataCollectionPass = @import("analysis/typedata_collection_pass.zig");
const collisionCollectionPass = @import("analysis/collision_collection_pass.zig");

pub fn runCollectionPasses(builder: *sema.Builder) void {
    idCollectionPass.collectTypeIds(builder);
    typeDataCollectionPass.collectTypeData(builder);
}

pub fn runCollectionOnGeneric(scope: *sema.Scope, block: *untyped.Block) void {
    idCollectionPass.collectTypeIdsFromBlock(scope, block, .public);
}

pub fn runCollisionCollection(builder: *sema.Builder) void {
    collisionCollectionPass.collectCollisionErrors(builder);
}