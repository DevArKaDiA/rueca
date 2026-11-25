enum Direction {
    case north, south

    func opposite() -> Direction {
        switch self {
        case .north: return .south
        case .south: return .north
        }
    }

    func flipped() -> Direction {
        return opposite()
    }
}

func turn(_ d: Direction) -> Direction {
    return d.flipped()
}

print(turn(.north))
