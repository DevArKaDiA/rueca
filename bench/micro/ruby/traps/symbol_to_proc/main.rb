class Row
  def cells
    [1]
  end
end

class Table
  def initialize
    @rows = []
  end

  def all_cells
    @rows.map(&:cells)
  end
end
