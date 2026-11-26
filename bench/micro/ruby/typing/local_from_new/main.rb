class Repo
  def save(x)
    x
  end
end

def run
  repo = Repo.new
  repo.save(1)
end
