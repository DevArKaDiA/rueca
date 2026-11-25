class Model
  def self.table
    "t"
  end
end

class Post < Model
  def self.query
    table
  end
end

def run
  Post.table
end
