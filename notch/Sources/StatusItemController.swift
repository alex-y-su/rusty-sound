import AppKit

/// Menu bar status item: an icon that shows the playback state, and a small
/// popover with album art, the current track, and transport buttons.
final class StatusItemController: NSObject {
    private enum DisplayState {
        case loading
        case off
        case failed(String)
        case ok(NowState)
    }

    private static let popoverSize = NSSize(width: 300, height: 160)
    private static let artSize: CGFloat = 56

    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let popover = NSPopover()

    private let artView = NSImageView()
    private let titleLabel = NSTextField(labelWithString: "")
    private let detailLabel = NSTextField(labelWithString: "")
    private let prevButton = NSButton(title: "", target: nil, action: nil)
    private let playButton = NSButton(title: "", target: nil, action: nil)
    private let nextButton = NSButton(title: "", target: nil, action: nil)
    private let radioButton = NSButton(title: "", target: nil, action: nil)
    private let resetButton = NSButton(title: "", target: nil, action: nil)

    private var state: DisplayState = .loading
    private var artworkURL: String?
    private var pollTimer: Timer?
    private var pollInFlight = false
    private let backend = DispatchQueue(label: "rusty-notch.cli")

    func start() {
        buildPopover()
        if let button = item.button {
            button.target = self
            button.action = #selector(togglePopover(_:))
        }
        render()
        pollTimer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            self?.poll()
        }
        poll()
    }

    // MARK: Polling and commands

    private func poll() {
        guard !pollInFlight else { return }
        pollInFlight = true
        backend.async {
            let result = RustySoundClient.now()
            DispatchQueue.main.async {
                self.pollInFlight = false
                switch result {
                case .success(let now): self.state = .ok(now)
                case .serviceOff: self.state = .off
                case .failure(let message): self.state = .failed(message)
                }
                self.render()
            }
        }
    }

    private func run(_ command: String, _ extra: [String] = []) {
        backend.async {
            _ = RustySoundClient.command(command, extra)
            DispatchQueue.main.async { self.poll() }
        }
    }

    // MARK: Rendering

    private func render() {
        let symbol: String
        var title = "Nothing playing"
        var detail = ""
        var artwork: String?
        var canControl = false
        var playSymbol = "play.fill"
        var playCommand = "play"
        var canNext = false
        var canPrev = false
        var radioOn = false

        switch state {
        case .loading:
            symbol = "music.note"
            title = "Loading…"
        case .off:
            symbol = "music.note.slash"
            title = "Service is off"
            detail = "Run `rusty-sound start`"
        case .failed(let message):
            symbol = "exclamationmark.triangle"
            title = "Error"
            detail = message
        case .ok(let now):
            if now.playing {
                symbol = now.paused ? "pause.fill" : "waveform"
                let parts = Self.splitNow(now.now)
                title = parts.title
                detail = [parts.artist, now.paused ? "Paused" : nil, "\(now.queued) queued"]
                    .compactMap { $0 }
                    .filter { !$0.isEmpty }
                    .joined(separator: " · ")
                artwork = now.artwork
                canNext = true
                canPrev = now.canPrev
                playSymbol = now.paused ? "play.fill" : "pause.fill"
                playCommand = now.paused ? "resume" : "pause"
            } else {
                symbol = "music.note"
                if let error = now.error {
                    title = "Stopped"
                    detail = error
                }
            }
            radioOn = now.radio
            canControl = true
        }

        if let button = item.button {
            let image = NSImage(systemSymbolName: symbol, accessibilityDescription: "Rusty Sound")
            image?.isTemplate = true
            button.image = image
            button.toolTip = [title, detail].filter { !$0.isEmpty }.joined(separator: " — ")
        }

        titleLabel.stringValue = title
        detailLabel.stringValue = detail
        setArtwork(artwork)

        setSymbol(playButton, playSymbol)
        playButton.identifier = NSUserInterfaceItemIdentifier(playCommand)
        playButton.isEnabled = canControl
        nextButton.isEnabled = canNext
        prevButton.isEnabled = canNext && canPrev
        prevButton.toolTip = canPrev ? "Previous track" : "Start of the queue"
        radioButton.isEnabled = canControl
        resetButton.isEnabled = canControl
        radioButton.contentTintColor = radioOn ? .controlAccentColor : .labelColor
        radioButton.identifier = NSUserInterfaceItemIdentifier(radioOn ? "radio-off" : "radio-on")
        radioButton.toolTip = radioOn ? "Radio on: queue follows related tracks" : "Radio off"
    }

    /// Splits "Title — Artist" at the last " — " separator.
    private static func splitNow(_ now: String?) -> (title: String, artist: String) {
        guard let now, let r = now.range(of: " — ", options: .backwards) else {
            return (now ?? "Unknown", "")
        }
        return (String(now[..<r.lowerBound]), String(now[r.upperBound...]))
    }

    /// Loads the album art once per track; keeps only the image for the current track.
    private func setArtwork(_ urlString: String?) {
        guard urlString != artworkURL else { return }
        artworkURL = urlString
        guard let urlString, let url = URL(string: urlString) else {
            artView.image = NSImage(systemSymbolName: "music.note", accessibilityDescription: nil)
            return
        }
        URLSession.shared.dataTask(with: url) { data, _, _ in
            guard let data, let image = NSImage(data: data) else { return }
            DispatchQueue.main.async {
                if self.artworkURL == urlString {
                    self.artView.image = image
                }
            }
        }.resume()
    }

    // MARK: Popover

    private func buildPopover() {
        let view = NSView(frame: NSRect(origin: .zero, size: Self.popoverSize))
        view.appearance = NSAppearance(named: .darkAqua)

        artView.imageScaling = .scaleProportionallyUpOrDown
        artView.wantsLayer = true
        artView.layer?.cornerRadius = 8
        artView.layer?.masksToBounds = true
        artView.translatesAutoresizingMaskIntoConstraints = false
        artView.widthAnchor.constraint(equalToConstant: Self.artSize).isActive = true
        artView.heightAnchor.constraint(equalToConstant: Self.artSize).isActive = true

        titleLabel.font = .systemFont(ofSize: 14, weight: .semibold)
        titleLabel.textColor = .labelColor
        titleLabel.lineBreakMode = .byTruncatingTail
        detailLabel.font = .systemFont(ofSize: 11.5)
        detailLabel.textColor = .secondaryLabelColor
        detailLabel.lineBreakMode = .byTruncatingTail
        let textStack = NSStackView(views: [titleLabel, detailLabel])
        textStack.orientation = .vertical
        textStack.alignment = .leading
        textStack.spacing = 3
        textStack.translatesAutoresizingMaskIntoConstraints = false
        textStack.widthAnchor.constraint(equalToConstant: 200).isActive = true

        let header = NSStackView(views: [artView, textStack])
        header.orientation = .horizontal
        header.alignment = .centerY
        header.spacing = 12

        configureButton(prevButton, symbol: "backward.fill", action: #selector(prevTapped(_:)))
        configureButton(playButton, symbol: "play.fill", action: #selector(playTapped(_:)))
        configureButton(nextButton, symbol: "forward.fill", action: #selector(nextTapped(_:)))
        configureButton(radioButton, symbol: "dot.radiowaves.left.and.right", action: #selector(radioTapped(_:)))
        configureButton(resetButton, symbol: "arrow.counterclockwise", action: #selector(resetTapped(_:)))
        resetButton.toolTip = "Start over from the top of your likes"

        let controls = NSStackView(views: [prevButton, playButton, nextButton, radioButton, resetButton])
        controls.orientation = .horizontal
        controls.alignment = .centerY
        controls.spacing = 16

        let stack = NSStackView(views: [header, controls])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 14
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            stack.centerYAnchor.constraint(equalTo: view.centerYAnchor),
        ])

        let vc = NSViewController()
        vc.view = view
        popover.contentViewController = vc
        popover.contentSize = Self.popoverSize
        popover.behavior = .transient
        popover.appearance = NSAppearance(named: .darkAqua)
    }

    private func configureButton(_ button: NSButton, symbol: String, action: Selector?) {
        button.target = self
        button.action = action
        button.isBordered = false
        button.imagePosition = .imageOnly
        button.contentTintColor = .labelColor
        button.translatesAutoresizingMaskIntoConstraints = false
        button.widthAnchor.constraint(equalToConstant: 30).isActive = true
        button.heightAnchor.constraint(equalToConstant: 30).isActive = true
        setSymbol(button, symbol)
    }

    private func setSymbol(_ button: NSButton, _ name: String) {
        button.image = NSImage(systemSymbolName: name, accessibilityDescription: nil)
        button.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 17, weight: .regular)
    }

    @objc private func togglePopover(_ sender: Any?) {
        if popover.isShown {
            popover.performClose(sender)
            return
        }
        guard let button = item.button else { return }
        poll()
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc private func playTapped(_ sender: Any?) {
        run(playButton.identifier?.rawValue ?? "play")
    }

    @objc private func nextTapped(_ sender: Any?) {
        run("next")
    }

    @objc private func prevTapped(_ sender: Any?) {
        run("prev")
    }

    @objc private func resetTapped(_ sender: Any?) {
        run("reset")
    }

    @objc private func radioTapped(_ sender: Any?) {
        // "radio-on" means radio is currently off, so tapping turns it on.
        let turningOn = radioButton.identifier?.rawValue == "radio-on"
        run("radio", turningOn ? [] : ["off"])
    }
}
