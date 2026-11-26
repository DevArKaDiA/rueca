class User
  def initialize(name)
    @name = name
  end
end

def build
  User.new("a")
end
