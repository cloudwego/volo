include "parent.thrift"
namespace go streamx.name_edges.renamed
namespace rs name_rust
typedef parent.Message Alias
service Original extends parent.Parent {
    Alias Copy(2: Alias request)
    void match()
    parent.Message OriginalCase(7: parent.Message request)
        (streaming.mode="client", pilota.name="handle_original")
} (pilota.name="Rewritten")
