namespace rs echo

struct Request {
        1: required string action,
        2: required string msg,
}

struct Response {
        1: required string action,
        2: required string msg,
}

struct SubMessage {
    1: optional i64 id;
    2: optional string value;
}
struct Message {
    1: optional i64 id;
    2: optional string value;
    3: optional list<SubMessage> subMessages;
}

// 复杂参数
struct ObjReq {
    1: required string action(api.path = 'action')
    2: required string msg(api.header = 'msg')
    3: required map<string, SubMessage> msgMap(api.body = 'msgMap')
    4: required list<SubMessage> subMsgs(api.body = 'subMsgs')
    5: optional set<Message> msgSet(api.body = 'msgSet')
    6: required Message flagMsg(api.body = 'flagMsg')
    7: optional string mockCost,
}

struct ObjResp {
    1: required string action(api.header = 'action')
    2: required string msg(api.header = 'msg')
    3: required map<string, SubMessage> msgMap(api.body = 'msgMap')
    4: required list<SubMessage> subMsgs(api.body = 'subMsgs')
    5: optional set<Message> msgSet(api.body = 'msgSet')
    6: required Message flagMsg(api.body = 'flagMsg')
}

service EchoServer {
    Response Echo(1: Request req)
    ObjResp TestObj(1: ObjReq req)(api.post = '/test/obj/:action', api.baseurl = 'example.com', api.param = 'true', api.serializer = 'json')
}
