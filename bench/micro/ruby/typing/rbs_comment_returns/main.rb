# typed: true
class Node
  def visit
    1
  end
end

class Tree
  #: -> Node
  def root
    Node.new
  end

  def walk
    root.visit
  end
end
