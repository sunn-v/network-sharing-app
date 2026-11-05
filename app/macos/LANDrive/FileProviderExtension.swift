// LAN Drive in Finder: a read-only File Provider. Listings and file bytes come from the
// Network Sharing app over its localhost bridge (lanshare_core::bridge); the port and token
// are in bridge.json inside the shared App Group container.
//
// Item ids are the bridge's: "root", "d/<peer>", "s/<peer>/<share>", "f/<peer>/<share>/<path>".

import FileProvider
import UniformTypeIdentifiers

// MARK: - Bridge client

struct BridgeItem: Decodable {
    let id: String
    let parent: String
    let name: String
    let is_dir: Bool
    let size: UInt64
    let modified: UInt64
}

enum Bridge {
    struct Config: Decodable { let port: Int; let token: String }

    static func config() throws -> Config {
        guard let group = Bundle.main.object(forInfoDictionaryKey: "LANDriveGroup") as? String,
              let dir = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
        else { throw NSFileProviderError(.serverUnreachable) }
        let data = try Data(contentsOf: dir.appendingPathComponent("bridge.json"))
        return try JSONDecoder().decode(Config.self, from: data)
    }

    static func request(_ path: String, id: String) throws -> URLRequest {
        let cfg = try config()
        // Encode everything but letters and digits; the bridge decodes %XX.
        let q = id.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? id
        var req = URLRequest(url: URL(string: "http://127.0.0.1:\(cfg.port)\(path)?id=\(q)")!)
        req.setValue("Bearer \(cfg.token)", forHTTPHeaderField: "Authorization")
        req.timeoutInterval = 60
        return req
    }

    static func check(_ response: URLResponse, _ body: Data?) throws {
        guard let http = response as? HTTPURLResponse else { throw NSFileProviderError(.serverUnreachable) }
        if http.statusCode == 200 { return }
        let text = body.flatMap { String(data: $0, encoding: .utf8) } ?? ""
        throw NSFileProviderError(text.contains("not found") ? .noSuchItem : .serverUnreachable)
    }

    static func items(_ id: String) async throws -> [BridgeItem] {
        let (data, response) = try await URLSession.shared.data(for: request("/items", id: id))
        try check(response, data)
        return try JSONDecoder().decode([BridgeItem].self, from: data)
    }

    static func item(_ id: String) async throws -> BridgeItem {
        let (data, response) = try await URLSession.shared.data(for: request("/item", id: id))
        try check(response, data)
        return try JSONDecoder().decode(BridgeItem.self, from: data)
    }

    /// Download a file's bytes to a temporary file this extension owns.
    static func download(_ id: String) async throws -> URL {
        let (tmp, response) = try await URLSession.shared.download(for: request("/read", id: id))
        try check(response, nil)
        let dest = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.moveItem(at: tmp, to: dest)
        return dest
    }
}

func providerError(_ error: Error) -> Error {
    if error is NSFileProviderError { return error }
    // The app is not running, or the other device is offline.
    return NSFileProviderError(.serverUnreachable)
}

// MARK: - Items

final class Item: NSObject, NSFileProviderItem {
    let b: BridgeItem
    init(_ b: BridgeItem) { self.b = b }

    static func ident(_ id: String) -> NSFileProviderItemIdentifier {
        id == "root" ? .rootContainer : NSFileProviderItemIdentifier(id)
    }

    static func bridgeId(_ ident: NSFileProviderItemIdentifier) -> String {
        ident == .rootContainer ? "root" : ident.rawValue
    }

    var itemIdentifier: NSFileProviderItemIdentifier { Item.ident(b.id) }
    var parentItemIdentifier: NSFileProviderItemIdentifier { Item.ident(b.parent) }
    var filename: String { b.name }
    var contentType: UTType {
        b.is_dir ? .folder : (UTType(filenameExtension: (b.name as NSString).pathExtension) ?? .data)
    }
    var documentSize: NSNumber? { b.is_dir ? nil : NSNumber(value: b.size) }
    var contentModificationDate: Date? { b.modified > 0 ? Date(timeIntervalSince1970: TimeInterval(b.modified)) : nil }
    // Read-only: Finder greys out rename, delete and editing in place.
    var capabilities: NSFileProviderItemCapabilities {
        b.is_dir ? [.allowsReading, .allowsContentEnumerating] : [.allowsReading]
    }
    var itemVersion: NSFileProviderItemVersion {
        let v = Data("\(b.size)-\(b.modified)".utf8)
        return NSFileProviderItemVersion(contentVersion: v, metadataVersion: v)
    }
}

// MARK: - Enumeration

/// Ids each container held when last listed, so a later pass can report what disappeared.
final class Known {
    static let shared = Known()
    private var ids: [String: Set<String>] = [:]
    private let lock = NSLock()

    /// Store the new listing; return ids that were there before and are gone now.
    func update(_ container: String, _ items: [BridgeItem]) -> [String] {
        lock.lock(); defer { lock.unlock() }
        let now = Set(items.map(\.id))
        let gone = ids[container, default: []].subtracting(now)
        ids[container] = now
        return Array(gone)
    }

    var containers: [String] {
        lock.lock(); defer { lock.unlock() }
        return Array(ids.keys)
    }
}

func anchor() -> NSFileProviderSyncAnchor {
    NSFileProviderSyncAnchor(Data(String(Date().timeIntervalSince1970).utf8))
}

final class Enumerator: NSObject, NSFileProviderEnumerator {
    /// nil = the working set.
    let container: String?
    init(container: String?) { self.container = container }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        guard let container else { observer.finishEnumerating(upTo: nil); return }
        Task {
            do {
                let items = try await Bridge.items(container)
                _ = Known.shared.update(container, items)
                observer.didEnumerate(items.map { Item($0) as NSFileProviderItem })
                observer.finishEnumerating(upTo: nil)
            } catch {
                observer.finishEnumeratingWithError(providerError(error))
            }
        }
    }

    // The app signals the working set; re-list every folder Finder has opened and report
    // what changed or went away.
    func enumerateChanges(for observer: NSFileProviderChangeObserver, from syncAnchor: NSFileProviderSyncAnchor) {
        let containers = container.map { [$0] } ?? Known.shared.containers
        Task {
            var updated: [NSFileProviderItem] = []
            var deleted: [NSFileProviderItemIdentifier] = []
            for c in containers {
                // A folder that can't be listed now (device offline) keeps what Finder already shows.
                guard let items = try? await Bridge.items(c) else { continue }
                deleted += Known.shared.update(c, items).map(Item.ident)
                updated += items.map { Item($0) as NSFileProviderItem }
            }
            if !updated.isEmpty { observer.didUpdate(updated) }
            if !deleted.isEmpty { observer.didDeleteItems(withIdentifiers: deleted) }
            observer.finishEnumeratingChanges(upTo: anchor(), moreComing: false)
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(anchor())
    }
}

// MARK: - Extension

final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    required init(domain: NSFileProviderDomain) { super.init() }

    func invalidate() {}

    func item(for identifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest,
              completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void) -> Progress {
        Task {
            do { completionHandler(Item(try await Bridge.item(Item.bridgeId(identifier))), nil) }
            catch { completionHandler(nil, providerError(error)) }
        }
        return Progress()
    }

    func fetchContents(for itemIdentifier: NSFileProviderItemIdentifier, version requestedVersion: NSFileProviderItemVersion?,
                       request: NSFileProviderRequest,
                       completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        Task {
            do {
                let id = Item.bridgeId(itemIdentifier)
                let item = try await Bridge.item(id)
                let url = try await Bridge.download(id)
                progress.completedUnitCount = 1
                completionHandler(url, Item(item), nil)
            } catch {
                completionHandler(nil, nil, providerError(error))
            }
        }
        return progress
    }

    // LAN Drive is read-only: nothing is written back to the other device.
    func createItem(basedOn itemTemplate: NSFileProviderItem, fields: NSFileProviderItemFields, contents url: URL?,
                    options: NSFileProviderCreateItemOptions = [], request: NSFileProviderRequest,
                    completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void) -> Progress {
        completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func modifyItem(_ item: NSFileProviderItem, baseVersion version: NSFileProviderItemVersion, changedFields: NSFileProviderItemFields,
                    contents newContents: URL?, options: NSFileProviderModifyItemOptions = [], request: NSFileProviderRequest,
                    completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void) -> Progress {
        completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func deleteItem(identifier: NSFileProviderItemIdentifier, baseVersion version: NSFileProviderItemVersion,
                    options: NSFileProviderDeleteItemOptions = [], request: NSFileProviderRequest,
                    completionHandler: @escaping (Error?) -> Void) -> Progress {
        completionHandler(NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func enumerator(for containerItemIdentifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest) throws -> NSFileProviderEnumerator {
        if containerItemIdentifier == .workingSet { return Enumerator(container: nil) }
        return Enumerator(container: Item.bridgeId(containerItemIdentifier))
    }
}
