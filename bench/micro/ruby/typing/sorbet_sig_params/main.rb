# typed: true
class Repo
  def save(x)
    x
  end
end

class Service
  extend T::Sig

  sig { params(repo: Repo).returns(Integer) }
  def handle(repo)
    repo.save(1)
  end
end
