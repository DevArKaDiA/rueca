class Repo
  def save(x)
    x
  end
end

class Service
  def initialize
    @repo = Repo.new
  end

  def handle(x)
    @repo.save(x)
  end
end
