// brief's Calendar helper: reads events from the Mac's Calendar app through EventKit and writes
// them as JSON. brief compiles this into ~/.brief/calendar/BriefCalendar.app, so macOS asks for
// (and remembers) Calendar access for "brief" itself, also when the background service runs it.
//
//   BriefCalendar --out FILE [--back DAYS] [--forward DAYS] [--ask]
//
// Without --ask it never shows the permission prompt; it just reports the current status.

import EventKit
import Foundation

var outPath = ""
var back = 2.0
var forward = 7.0
var ask = false
var it = CommandLine.arguments.dropFirst().makeIterator()
while let a = it.next() {
    switch a {
    case "--out": outPath = it.next() ?? ""
    case "--back": back = Double(it.next() ?? "") ?? back
    case "--forward": forward = Double(it.next() ?? "") ?? forward
    case "--ask": ask = true
    default: break
    }
}

func finish(_ obj: [String: Any]) -> Never {
    let data = (try? JSONSerialization.data(withJSONObject: obj, options: [])) ?? Data("{}".utf8)
    if outPath.isEmpty {
        FileHandle.standardOutput.write(data)
    } else {
        try? data.write(to: URL(fileURLWithPath: outPath), options: .atomic)
    }
    exit(0)
}

let store = EKEventStore()
var status = EKEventStore.authorizationStatus(for: .event)
if status == .notDetermined && ask {
    let sem = DispatchSemaphore(value: 0)
    store.requestFullAccessToEvents { _, _ in sem.signal() }
    sem.wait()
    status = EKEventStore.authorizationStatus(for: .event)
}
guard status == .fullAccess else {
    let name: String
    switch status {
    case .notDetermined: name = "not_determined"
    case .writeOnly: name = "write_only"
    case .restricted: name = "restricted"
    default: name = "denied"
    }
    finish(["status": name])
}

let iso = ISO8601DateFormatter()
iso.timeZone = .current
iso.formatOptions = [.withInternetDateTime]

let cal = Calendar.current
let startOfToday = cal.startOfDay(for: Date())
let start = startOfToday.addingTimeInterval(-back * 86400)
let end = startOfToday.addingTimeInterval((forward + 1) * 86400)
let calendars = store.calendars(for: .event)
let events = store.events(matching: store.predicateForEvents(withStart: start, end: end, calendars: calendars))
    .sorted { $0.startDate < $1.startDate }

var out: [[String: Any]] = []
for e in events {
    let me = e.attendees?.first { $0.isCurrentUser }
    if me?.participantStatus == .declined { continue }
    var item: [String: Any] = [
        "title": e.title ?? "",
        "start": iso.string(from: e.startDate),
        "end": iso.string(from: e.endDate),
        "all_day": e.isAllDay,
        "calendar": e.calendar?.title ?? "",
        "attendees": e.attendees?.count ?? 0,
        "free": e.availability == .free,
    ]
    if let l = e.location, !l.isEmpty { item["location"] = l }
    if let n = e.notes, !n.isEmpty { item["notes"] = String(n.prefix(300)) }
    if me?.participantStatus == .pending || me?.participantStatus == .tentative { item["tentative"] = true }
    out.append(item)
}
finish(["status": "granted", "calendars": calendars.map { $0.title }, "events": out])
