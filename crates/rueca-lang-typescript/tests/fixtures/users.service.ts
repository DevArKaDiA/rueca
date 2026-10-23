import { Injectable, NotFoundException } from '@nestjs/common';
import { InjectRepository } from '@nestjs/typeorm';
import type { Repository } from 'typeorm';
import { User } from './user.entity';
import { CreateUserDto, type UpdateUserDto } from './dto';
import * as crypto from 'crypto';

export const DEFAULT_PAGE_SIZE = 20;

/** Paginated result. */
export interface Page<T> {
  items: T[];
  total: number;
  next(cursor: string): Promise<Page<T>>;
}

export type UserId = string;

export enum Role {
  Admin = 'admin',
  Member = 'member',
}

export abstract class BaseService<T> {
  protected abstract readonly name: string;
  abstract findById(id: string): Promise<T | null>;
}

/**
 * Users domain service.
 * Handles persistence.
 */
@Injectable()
export class UserService extends BaseService<User> implements Searchable<User>, Auditable {
  protected readonly name = 'users';
  private cache = new Map<UserId, User>();

  constructor(
    @InjectRepository(User) private readonly repo: Repository<User>,
    private readonly audit: AuditLog,
  ) {
    super();
  }

  /** Finds a user or throws. */
  async findById(id: UserId): Promise<User | null> {
    const cached = this.cache.get(id);
    if (cached) {
      return cached;
    }
    const user = await this.repo.findOne({ where: { id } });
    if (!user) {
      throw new NotFoundException(`user ${id}`);
    }
    this.audit?.record('read', user.id);
    return user;
  }

  async create(dto: CreateUserDto): Promise<User> {
    const { email, profile: { displayName } } = dto;
    const user = this.repo.create({ email, displayName, token: crypto.randomUUID() });
    this.lastCreated = user;
    for (const hook of this.hooks) {
      hook(user);
    }
    try {
      return await this.repo.save(user);
    } catch (err) {
      logError(err, DEFAULT_PAGE_SIZE);
      throw err;
    }
  }

  update(id: UserId, dto: UpdateUserDto) {
    return this.repo.update(id, dto satisfies Partial<User>) as Promise<unknown>;
  }

  get size(): number {
    return this.cache.size;
  }
  set size(v: number) {
    this.cache.clear();
  }
}
