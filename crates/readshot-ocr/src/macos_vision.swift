import Foundation
import Vision

@_cdecl("readshot_vision_recognize_png")
public func readshotVisionRecognizePng(
    _ pngBytes: UnsafePointer<UInt8>?,
    _ pngLen: Int,
    _ languages: UnsafePointer<UnsafePointer<CChar>?>?,
    _ languageCount: Int,
    _ useLanguageCorrection: Bool,
    _ outJson: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    _ outError: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    outJson?.pointee = nil
    outError?.pointee = nil

    guard let pngBytes, pngLen > 0 else {
        setCString(outError, "empty image data")
        return 1
    }

    let imageData = Data(bytes: pngBytes, count: pngLen)
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = useLanguageCorrection
    request.automaticallyDetectsLanguage = languageCount == 0

    if languageCount > 0 {
        var preferredLanguages: [String] = []
        for index in 0..<languageCount {
            guard let languagePointer = languages?[index] else {
                continue
            }
            preferredLanguages.append(String(cString: languagePointer))
        }
        request.recognitionLanguages = preferredLanguages
    }

    do {
        try VNImageRequestHandler(data: imageData, options: [:]).perform([request])
        let payload = try visionPayload(from: request.results ?? [])
        setCString(outJson, payload)
        return 0
    } catch {
        setCString(outError, error.localizedDescription)
        return 1
    }
}

@_cdecl("readshot_vision_free_string")
public func readshotVisionFreeString(_ string: UnsafeMutablePointer<CChar>?) {
    free(string)
}

private func visionPayload(from observations: [VNRecognizedTextObservation]) throws -> String {
    var textLines: [String] = []
    var lineObjects: [[String: Any]] = []
    var totalConfidence: Float = 0
    var counted: Float = 0

    for observation in observations {
        guard let candidate = observation.topCandidates(1).first, !candidate.string.isEmpty else {
            continue
        }

        let box = observation.boundingBox
        textLines.append(candidate.string)
        lineObjects.append([
            "text": candidate.string,
            "x": Float(box.origin.x),
            "y": topY(box),
            "w": Float(box.size.width),
            "h": Float(box.size.height),
            "words": wordObjects(for: candidate),
        ])
        totalConfidence += candidate.confidence
        counted += 1
    }

    let payload: [String: Any] = [
        "text": textLines.joined(separator: "\n"),
        "average_confidence": counted == 0 ? 0 : totalConfidence / counted,
        "lines": lineObjects,
    ]
    let jsonData = try JSONSerialization.data(withJSONObject: payload, options: [])
    guard let json = String(data: jsonData, encoding: .utf8) else {
        throw VisionShimError.invalidUtf8
    }
    return json
}

/// Vision boxes are normalised with a bottom-left origin; Readshot's are
/// top-left.
private func topY(_ box: CGRect) -> Float {
    Float(min(max(1.0 - box.origin.y - box.size.height, 0.0), 1.0))
}

/// One box per whitespace-separated word. `start` / `end` count Unicode
/// scalars, which is what Rust `char` indices count.
private func wordObjects(for candidate: VNRecognizedText) -> [[String: Any]] {
    let string = candidate.string
    let scalars = string.unicodeScalars
    var words: [[String: Any]] = []
    for token in string.split(whereSeparator: { $0.isWhitespace }) {
        let range = token.startIndex..<token.endIndex
        guard let observation = try? candidate.boundingBox(for: range) else {
            continue
        }
        let box = observation.boundingBox
        words.append([
            "start": scalars.distance(from: scalars.startIndex, to: range.lowerBound),
            "end": scalars.distance(from: scalars.startIndex, to: range.upperBound),
            "x": Float(box.origin.x),
            "y": topY(box),
            "w": Float(box.size.width),
            "h": Float(box.size.height),
        ])
    }
    return words
}

private func setCString(
    _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    _ value: String
) {
    output?.pointee = strdup(value)
}

private enum VisionShimError: LocalizedError {
    case invalidUtf8

    var errorDescription: String? {
        switch self {
        case .invalidUtf8:
            return "Vision result JSON was not UTF-8"
        }
    }
}
